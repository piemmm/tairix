//! `TREE_CONNECT` (MS-SMB2 §2.2.9, §2.2.10).

use alloc::string::String;

use crate::header::{structure, HEADER_LEN};
use crate::wire::{buffer_at, from_utf16le, utf16le, utf16le_len, Encoder};
use crate::SmbError;

/// The fixed part of a `TREE_CONNECT` request's body.
const REQUEST_FIXED: usize = 8;

/// The longest share name, in characters (MS-SRVS §2.2.2.10's
/// `NAMETYPE_SHARE`).
pub const MAX_SHARE_NAME: usize = 80;

/// Whether `name` is a share a tree connect can name: one to
/// [`MAX_SHARE_NAME`] characters, none a path separator or a control
/// character.
#[must_use]
pub fn is_share_name(name: &str) -> bool {
    let mut count = 0;
    for c in name.chars() {
        if matches!(c, '\\' | '/') || c.is_control() {
            return false;
        }
        count += 1;
    }
    (1..=MAX_SHARE_NAME).contains(&count)
}

/// The longest share path a request may carry: `\\server\share`, bounded by
/// what one name component and a host may be.
const MAX_PATH_BYTES: usize = 1024;

/// A `TREE_CONNECT` request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeConnectRequest {
    /// The share, as `\\server\share`.
    pub path: String,
}

impl TreeConnectRequest {
    /// Append the request's body after its header. A path past the bound
    /// latches [`SmbError::TooLarge`]: what this engine sends it would also
    /// accept.
    pub fn encode(&self, enc: &mut Encoder) {
        let path_len = utf16le_len(&self.path);
        if path_len > MAX_PATH_BYTES {
            enc.fail(SmbError::TooLarge);
        }
        enc.u16(9);
        enc.u16(0);
        let offset = enc.u16_of(HEADER_LEN + REQUEST_FIXED);
        enc.u16(offset);
        let len = enc.u16_of(path_len);
        enc.u16(len);
        utf16le(&self.path, enc);
    }

    /// The request in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed request,
    /// [`SmbError::TooLarge`] for a path past the bound.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 9)?;
        r.skip(2)?;
        let offset = usize::from(r.u16()?);
        let len = usize::from(r.u16()?);
        if len > MAX_PATH_BYTES {
            return Err(SmbError::TooLarge);
        }
        Ok(Self {
            path: from_utf16le(buffer_at(message, offset, len)?)?,
        })
    }
}

/// What a share is (MS-SMB2 §2.2.10).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ShareType {
    /// A filesystem share.
    Disk,
    /// A named-pipe share (`IPC$`).
    Pipe,
    /// A printer share.
    Print,
}

impl ShareType {
    /// The type's code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Disk => 1,
            Self::Pipe => 2,
            Self::Print => 3,
        }
    }

    /// The type with code `code`.
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Disk),
            2 => Some(Self::Pipe),
            3 => Some(Self::Print),
            _ => None,
        }
    }
}

/// A share flag: the server requires the share's messages encrypted.
pub const SHARE_FLAG_ENCRYPT_DATA: u32 = 0x0000_8000;

/// A share flag: the share is in a DFS namespace.
pub const SHARE_FLAG_DFS: u32 = 0x0000_0001;

/// A `TREE_CONNECT` response.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TreeConnectResponse {
    /// What the share is.
    pub share_type: ShareType,
    /// The share's flags.
    pub share_flags: u32,
    /// The share's capabilities.
    pub capabilities: u32,
    /// The most access the session may be granted on the share.
    pub maximal_access: u32,
}

impl TreeConnectResponse {
    /// Append the response's body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u16(16);
        enc.u8(self.share_type.code());
        enc.u8(0);
        enc.u32(self.share_flags);
        enc.u32(self.capabilities);
        enc.u32(self.maximal_access);
    }

    /// The response in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed response or names
    /// a share type SMB2 does not define.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 16)?;
        let share_type = ShareType::from_code(r.u8()?).ok_or(SmbError::Malformed)?;
        r.skip(1)?;
        Ok(Self {
            share_type,
            share_flags: r.u32()?,
            capabilities: r.u32()?,
            maximal_access: r.u32()?,
        })
    }
}
