//! `READ`, `WRITE`, `FLUSH` and `CLOSE` (MS-SMB2 §2.2.15–§2.2.22).
//!
//! Data is never copied here: a read's data is borrowed from the response it
//! arrived in, and a write's is written straight into the frame it is sent
//! in. Both are bounded by [`MAX_IO`].

use crate::file::{FileAttributes, FileId, FileTimes, MAX_IO};
use crate::header::{structure, HEADER_LEN};
use crate::wire::{buffer_at, Encoder};
use crate::SmbError;

/// Where a read response's data starts: just past its fixed part, which is
/// where this client asks the server to put it.
const READ_DATA_AT: u8 = 0x50;

/// The fixed part of a `WRITE` request's body, which its data follows.
const WRITE_FIXED: usize = 48;

/// The write flag asking for write-through.
const WRITE_THROUGH: u32 = 0x0000_0001;

/// The close flag asking for the object's attributes back.
const POSTQUERY_ATTRIB: u16 = 0x0001;

/// A `READ` request.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ReadRequest {
    /// The offset into the response this client asks the data placed at.
    pub padding: u8,
    /// How many bytes to read.
    pub length: u32,
    /// Where in the file to read from.
    pub offset: u64,
    /// The open to read.
    pub file_id: FileId,
    /// The fewest bytes that count as success.
    pub minimum: u32,
}

impl ReadRequest {
    /// A read of `length` bytes at `offset` of `file_id`, the data placed
    /// just past the response's fixed part.
    #[must_use]
    pub const fn new(file_id: FileId, offset: u64, length: u32) -> Self {
        Self {
            padding: READ_DATA_AT,
            length,
            offset,
            file_id,
            minimum: 0,
        }
    }

    /// Append the request's body after its header. A length past
    /// [`MAX_IO`] latches [`SmbError::TooLarge`].
    pub fn encode(&self, enc: &mut Encoder) {
        if usize::try_from(self.length).map_or(true, |len| len > MAX_IO) {
            enc.fail(SmbError::TooLarge);
        }
        enc.u16(49);
        enc.u8(self.padding);
        enc.u8(0);
        enc.u32(self.length);
        enc.u64(self.offset);
        enc.bytes(&self.file_id);
        enc.u32(self.minimum);
        enc.u32(0);
        enc.u32(0);
        enc.u16(0);
        enc.u16(0);
        enc.u8(0);
    }

    /// The request in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed request,
    /// [`SmbError::TooLarge`] past [`MAX_IO`].
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 49)?;
        let padding = r.u8()?;
        r.skip(1)?;
        let length = r.u32()?;
        if usize::try_from(length).map_or(true, |len| len > MAX_IO) {
            return Err(SmbError::TooLarge);
        }
        Ok(Self {
            padding,
            length,
            offset: r.u64()?,
            file_id: r.array()?,
            minimum: r.u32()?,
        })
    }
}

/// A `READ` response: the data, borrowed from the message it arrived in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ReadResponse<'a> {
    /// The data read.
    pub data: &'a [u8],
}

impl<'a> ReadResponse<'a> {
    /// Append the response's body after its header, the data where this
    /// client asks for it.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u16(17);
        enc.u8(READ_DATA_AT);
        enc.u8(0);
        let len = enc.u32_of(self.data.len());
        enc.u32(len);
        enc.u32(0);
        enc.u32(0);
        enc.buffer(self.data);
    }

    /// The response in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed response or puts
    /// its data inside its own fixed part, [`SmbError::TooLarge`] past
    /// [`MAX_IO`].
    pub fn decode(message: &'a [u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 17)?;
        let offset = usize::from(r.u8()?);
        r.skip(1)?;
        let len = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        if len > MAX_IO {
            return Err(SmbError::TooLarge);
        }
        if len > 0 && offset < usize::from(READ_DATA_AT) {
            return Err(SmbError::Malformed);
        }
        Ok(Self {
            data: buffer_at(message, offset, len)?,
        })
    }
}

/// A `WRITE` request: the data, borrowed from wherever the caller holds it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct WriteRequest<'a> {
    /// Where in the file to write.
    pub offset: u64,
    /// The open to write.
    pub file_id: FileId,
    /// Whether the write reaches storage before it completes.
    pub write_through: bool,
    /// The data.
    pub data: &'a [u8],
}

impl<'a> WriteRequest<'a> {
    /// Append the request's body after its header, the data straight after
    /// it. Data past [`MAX_IO`] latches [`SmbError::TooLarge`].
    pub fn encode(&self, enc: &mut Encoder) {
        if self.data.len() > MAX_IO {
            enc.fail(SmbError::TooLarge);
        }
        enc.u16(49);
        let data_offset = enc.u16_of(HEADER_LEN + WRITE_FIXED);
        enc.u16(data_offset);
        let len = enc.u32_of(self.data.len());
        enc.u32(len);
        enc.u64(self.offset);
        enc.bytes(&self.file_id);
        enc.u32(0);
        enc.u32(0);
        // Windows points the empty channel information at the data.
        enc.u16(data_offset);
        enc.u16(0);
        enc.u32(if self.write_through { WRITE_THROUGH } else { 0 });
        enc.buffer(self.data);
    }

    /// The request in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed request,
    /// [`SmbError::TooLarge`] past [`MAX_IO`].
    pub fn decode(message: &'a [u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 49)?;
        let data_offset = usize::from(r.u16()?);
        let len = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        if len > MAX_IO {
            return Err(SmbError::TooLarge);
        }
        let offset = r.u64()?;
        let file_id = r.array()?;
        r.skip(12)?;
        let flags = r.u32()?;
        Ok(Self {
            offset,
            file_id,
            write_through: flags & WRITE_THROUGH != 0,
            data: buffer_at(message, data_offset, len)?,
        })
    }
}

/// A `WRITE` response.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct WriteResponse {
    /// How many bytes were written.
    pub count: u32,
}

impl WriteResponse {
    /// Append the response's body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u16(17);
        enc.u16(0);
        enc.u32(self.count);
        enc.u32(0);
        enc.u16(0);
        enc.u16(0);
    }

    /// The response in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed response.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 17)?;
        r.skip(2)?;
        Ok(Self { count: r.u32()? })
    }
}

/// A `FLUSH` request; its response is the four-byte body
/// ([`crate::simple::decode_empty`]).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FlushRequest {
    /// The open to flush.
    pub file_id: FileId,
}

impl FlushRequest {
    /// Append the request's body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u16(24);
        enc.u16(0);
        enc.u32(0);
        enc.bytes(&self.file_id);
    }

    /// The request in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed request.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 24)?;
        r.skip(6)?;
        Ok(Self {
            file_id: r.array()?,
        })
    }
}

/// A `CLOSE` request.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CloseRequest {
    /// Whether to have the object's attributes back.
    pub post_query: bool,
    /// The open to close.
    pub file_id: FileId,
}

impl CloseRequest {
    /// Append the request's body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u16(24);
        enc.u16(if self.post_query { POSTQUERY_ATTRIB } else { 0 });
        enc.u32(0);
        enc.bytes(&self.file_id);
    }

    /// The request in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed request.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 24)?;
        let flags = r.u16()?;
        r.skip(4)?;
        Ok(Self {
            post_query: flags & POSTQUERY_ATTRIB != 0,
            file_id: r.array()?,
        })
    }
}

/// An object's attributes as a close reports them.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ClosedAttributes {
    /// Its times.
    pub times: FileTimes,
    /// The bytes allocated to it.
    pub allocation_size: u64,
    /// Its size.
    pub end_of_file: u64,
    /// Its attributes.
    pub attributes: FileAttributes,
}

/// A `CLOSE` response: the attributes, when the close asked for them.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CloseResponse {
    /// The object's attributes, when the server reported them.
    pub attributes: Option<ClosedAttributes>,
}

impl CloseResponse {
    /// Append the response's body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u16(60);
        if let Some(closed) = &self.attributes {
            enc.u16(POSTQUERY_ATTRIB);
            enc.u32(0);
            closed.times.write(enc);
            enc.u64(closed.allocation_size);
            enc.u64(closed.end_of_file);
            enc.u32(closed.attributes.bits());
        } else {
            enc.u16(0);
            enc.zeros(56);
        }
    }

    /// The response in `message`, header included. Attributes a server
    /// sends unflagged are not read, as MS-SMB2 requires.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed response.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 60)?;
        let flags = r.u16()?;
        r.skip(4)?;
        if flags & POSTQUERY_ATTRIB == 0 {
            r.skip(52)?;
            return Ok(Self { attributes: None });
        }
        Ok(Self {
            attributes: Some(ClosedAttributes {
                times: FileTimes::read(&mut r)?,
                allocation_size: r.u64()?,
                end_of_file: r.u64()?,
                attributes: FileAttributes::from_bits(r.u32()?),
            }),
        })
    }
}

#[cfg(test)]
#[path = "io_tests.rs"]
mod tests;
