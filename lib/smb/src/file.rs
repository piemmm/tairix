//! What a client does on a connected tree (MS-SMB2 §2.2.13–§2.2.40), and the
//! information classes those requests carry (MS-FSCC §2.4–§2.7).
//!
//! Every decoder is total and bounded. A name crosses only when TAIRiX and SMB
//! can both carry it ([`name`]), and a time only exactly: a `Time64` written
//! here must sit on FILETIME's 100 nanoseconds, never be rounded onto them.

use tairix_abi::time::Time64;

use crate::wire::{filetime_exact, time_of_filetime, Encoder, Reader};
use crate::SmbError;

pub mod create;
pub mod dir;
pub mod info;
pub mod io;
pub mod lease;
pub mod name;
pub mod notify;

/// A file id: the persistent and volatile halves a `CREATE` returns
/// (MS-SMB2 §2.2.14.1).
pub type FileId = [u8; 16];

/// The file id a request about no open file carries.
pub const NO_FILE: FileId = [0xff; 16];

/// The most data one `READ` or `WRITE` moves: a bound on what a server can
/// make the client buffer.
pub const MAX_IO: usize = 1 << 20;

/// The payload one credit covers (MS-SMB2 §3.2.4.1.5).
const CREDIT_BYTES: usize = 64 << 10;

/// The credits a request moving `payload` bytes either way consumes: one per
/// 64 KiB, and at least one.
#[must_use]
pub fn credit_charge(payload: usize) -> u16 {
    u16::try_from(payload.saturating_sub(1) / CREDIT_BYTES + 1).unwrap_or(u16::MAX)
}

flags! {
    /// File attributes (MS-FSCC §2.6).
    pub struct FileAttributes(u32) {
        /// Read-only.
        const READONLY = 0x0000_0001;
        /// Hidden from an ordinary listing.
        const HIDDEN = 0x0000_0002;
        /// The operating system's own.
        const SYSTEM = 0x0000_0004;
        /// A directory.
        const DIRECTORY = 0x0000_0010;
        /// Due to be archived.
        const ARCHIVE = 0x0000_0020;
        /// No other attribute: what a create asks for when it asks for none.
        const NORMAL = 0x0000_0080;
        /// Temporary storage.
        const TEMPORARY = 0x0000_0100;
        /// Sparse.
        const SPARSE_FILE = 0x0000_0200;
        /// A reparse point: a link, or something the server resolves itself.
        const REPARSE_POINT = 0x0000_0400;
        /// Its data is offline.
        const OFFLINE = 0x0000_1000;
    }
}

/// A file's four times.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FileTimes {
    /// When it was created.
    pub created: Time64,
    /// When it was last read.
    pub accessed: Time64,
    /// When its data was last written.
    pub written: Time64,
    /// When it last changed at all.
    pub changed: Time64,
}

impl FileTimes {
    pub(crate) fn read(r: &mut Reader<'_>) -> Result<Self, SmbError> {
        Ok(Self {
            created: read_filetime(r)?,
            accessed: read_filetime(r)?,
            written: read_filetime(r)?,
            changed: read_filetime(r)?,
        })
    }

    pub(crate) fn write(&self, enc: &mut Encoder) {
        for time in [self.created, self.accessed, self.written, self.changed] {
            write_filetime(enc, time);
        }
    }
}

pub(crate) fn read_filetime(r: &mut Reader<'_>) -> Result<Time64, SmbError> {
    time_of_filetime(r.u64()?)
}

/// Write `time` as a FILETIME, latching [`SmbError::TimeOutOfRange`] for one
/// FILETIME cannot carry exactly.
pub(crate) fn write_filetime(enc: &mut Encoder, time: Time64) {
    match filetime_exact(time) {
        Ok(filetime) => enc.u64(filetime),
        Err(err) => enc.fail(err),
    }
}

/// A reader over `data`, which must be exactly `len` bytes: the length of a
/// fixed-size structure a stranger sent.
pub(crate) fn exactly(data: &[u8], len: usize) -> Result<Reader<'_>, SmbError> {
    if data.len() == len {
        Ok(Reader::new(data))
    } else {
        Err(SmbError::Malformed)
    }
}

/// A body whose bytes after its structure size `fixed` are an output
/// buffer named by a 16-bit offset and 32-bit length: what `QUERY_DIRECTORY`,
/// `QUERY_INFO` and `CHANGE_NOTIFY` answer with.
pub(crate) fn output_buffer(message: &[u8], structure: u16, max: usize) -> Result<&[u8], SmbError> {
    let mut r = crate::header::structure(message, structure)?;
    let offset = usize::from(r.u16()?);
    let len = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
    if len > max {
        return Err(SmbError::TooLarge);
    }
    crate::wire::buffer_at(message, offset, len)
}

/// Append an output-buffer body: structure size 9, the offset, the length,
/// and the bytes.
pub(crate) fn write_output_buffer(enc: &mut Encoder, output: &[u8]) {
    enc.u16(9);
    let offset = enc.u16_of(crate::header::HEADER_LEN + 8);
    enc.u16(offset);
    let len = enc.u32_of(output.len());
    enc.u32(len);
    enc.buffer(output);
}

#[cfg(test)]
#[path = "file_tests.rs"]
mod tests;
