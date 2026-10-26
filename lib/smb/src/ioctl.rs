//! `IOCTL` (MS-SMB2 §2.2.31, §2.2.32).

use alloc::vec::Vec;

use crate::header::{structure, HEADER_LEN};
use crate::wire::{buffer_at, owned, Encoder};
use crate::SmbError;

/// A file id: the persistent and volatile halves a `CREATE` returns.
pub type FileId = [u8; 16];

/// The file id a request about no open file carries.
pub const NO_FILE: FileId = [0xff; 16];

/// The flag marking the control code a filesystem control.
pub const IS_FSCTL: u32 = 0x0000_0001;

/// The fixed part of an `IOCTL` request's body, which its input follows.
const REQUEST_FIXED: usize = 56;

/// The fixed part of an `IOCTL` response's body.
const RESPONSE_FIXED: usize = 48;

/// The most output a response may carry before it is refused.
const MAX_OUTPUT: usize = 1 << 20;

/// An `IOCTL` request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IoctlRequest {
    /// The control code.
    pub ctl_code: u32,
    /// The open the control is about, or [`NO_FILE`].
    pub file_id: FileId,
    /// The input buffer.
    pub input: Vec<u8>,
    /// The most output the client will take.
    pub max_output_response: u32,
    /// [`IS_FSCTL`] for a filesystem control.
    pub flags: u32,
}

impl IoctlRequest {
    /// Append the request's body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u16(57);
        enc.u16(0);
        enc.u32(self.ctl_code);
        enc.bytes(&self.file_id);
        let input_offset = if self.input.is_empty() {
            0
        } else {
            enc.u32_of(HEADER_LEN + REQUEST_FIXED)
        };
        enc.u32(input_offset);
        let input_len = enc.u32_of(self.input.len());
        enc.u32(input_len);
        enc.u32(0);
        enc.u32(0);
        enc.u32(0);
        enc.u32(self.max_output_response);
        enc.u32(self.flags);
        enc.u32(0);
        enc.bytes(&self.input);
    }

    /// The request in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed request.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 57)?;
        r.skip(2)?;
        let ctl_code = r.u32()?;
        let file_id = r.array()?;
        let input_offset = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        let input_len = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        r.skip(12)?;
        let max_output_response = r.u32()?;
        let flags = r.u32()?;
        Ok(Self {
            ctl_code,
            file_id,
            input: owned(buffer_at(message, input_offset, input_len)?)?,
            max_output_response,
            flags,
        })
    }
}

/// An `IOCTL` response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IoctlResponse {
    /// The control code answered.
    pub ctl_code: u32,
    /// The open it was about.
    pub file_id: FileId,
    /// The output buffer.
    pub output: Vec<u8>,
}

impl IoctlResponse {
    /// Append the response's body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u16(49);
        enc.u16(0);
        enc.u32(self.ctl_code);
        enc.bytes(&self.file_id);
        enc.u32(0);
        enc.u32(0);
        let output_offset = if self.output.is_empty() {
            0
        } else {
            enc.u32_of(HEADER_LEN + RESPONSE_FIXED)
        };
        enc.u32(output_offset);
        let output_len = enc.u32_of(self.output.len());
        enc.u32(output_len);
        enc.u32(0);
        enc.u32(0);
        enc.bytes(&self.output);
    }

    /// The response in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed response,
    /// [`SmbError::TooLarge`] past the output bound.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 49)?;
        r.skip(2)?;
        let ctl_code = r.u32()?;
        let file_id = r.array()?;
        r.skip(8)?;
        let output_offset = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        let output_len = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        if output_len > MAX_OUTPUT {
            return Err(SmbError::TooLarge);
        }
        Ok(Self {
            ctl_code,
            file_id,
            output: owned(buffer_at(message, output_offset, output_len)?)?,
        })
    }
}
