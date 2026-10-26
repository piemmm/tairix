//! The little-endian field codec every SMB2 message is built from.
//!
//! Offsets inside a message count from the start of its 64-byte header, as
//! MS-SMB2 defines them, so a buffer is located with [`buffer_at`] over the
//! whole message rather than its body.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::Time64;

use crate::SmbError;

/// A 16-byte GUID as it travels: a client, a server, a create context.
pub type Guid = [u8; 16];

/// Reads fields in order from a slice; every read is bounds-checked.
#[derive(Clone, Debug)]
pub struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    /// A reader at the start of `buf`.
    #[must_use]
    pub const fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }

    /// A reader at `offset` into `buf`.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] past the end.
    pub fn at(buf: &'a [u8], offset: usize) -> Result<Self, SmbError> {
        if offset > buf.len() {
            return Err(SmbError::Malformed);
        }
        Ok(Self { buf, at: offset })
    }

    /// How far into the slice the reader is.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.at
    }

    /// How many bytes are left.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.buf.len() - self.at
    }

    /// The next `len` bytes.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when fewer remain.
    pub fn bytes(&mut self, len: usize) -> Result<&'a [u8], SmbError> {
        let end = self.at.checked_add(len).ok_or(SmbError::Malformed)?;
        let bytes = self.buf.get(self.at..end).ok_or(SmbError::Malformed)?;
        self.at = end;
        Ok(bytes)
    }

    /// The next `N` bytes as an array.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when fewer remain.
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N], SmbError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.bytes(N)?);
        Ok(out)
    }

    /// Skip `len` bytes.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when fewer remain.
    pub fn skip(&mut self, len: usize) -> Result<(), SmbError> {
        self.bytes(len).map(|_| ())
    }

    /// A byte.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] at the end.
    pub fn u8(&mut self) -> Result<u8, SmbError> {
        Ok(self.array::<1>()?[0])
    }

    /// A little-endian `u16`.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when fewer than two bytes remain.
    pub fn u16(&mut self) -> Result<u16, SmbError> {
        self.array().map(u16::from_le_bytes)
    }

    /// A little-endian `u32`.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when fewer than four bytes remain.
    pub fn u32(&mut self) -> Result<u32, SmbError> {
        self.array().map(u32::from_le_bytes)
    }

    /// A little-endian `u64`.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when fewer than eight bytes remain.
    pub fn u64(&mut self) -> Result<u64, SmbError> {
        self.array().map(u64::from_le_bytes)
    }

    /// A GUID.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when fewer than sixteen bytes remain.
    pub fn guid(&mut self) -> Result<Guid, SmbError> {
        self.array()
    }
}

/// The `len` bytes `offset` bytes into `message`. A zero length names no
/// bytes, whatever its offset says, as MS-SMB2 allows a sender to leave it.
///
/// # Errors
///
/// [`SmbError::Malformed`] when the range leaves the message.
pub fn buffer_at(message: &[u8], offset: usize, len: usize) -> Result<&[u8], SmbError> {
    if len == 0 {
        return Ok(&[]);
    }
    let end = offset.checked_add(len).ok_or(SmbError::Malformed)?;
    message.get(offset..end).ok_or(SmbError::Malformed)
}

/// `bytes` copied into a buffer of their own.
///
/// # Errors
///
/// [`SmbError::OutOfMemory`] when the buffer cannot be had.
pub fn owned(bytes: &[u8]) -> Result<Vec<u8>, SmbError> {
    let mut out = Vec::new();
    out.try_reserve_exact(bytes.len())
        .map_err(|_| SmbError::OutOfMemory)?;
    out.extend_from_slice(bytes);
    Ok(out)
}

/// Appends fields to a buffer.
///
/// A failed write latches: every later write is ignored and
/// [`Encoder::finish`] reports the first failure, so a message is built with
/// plain calls and checked once.
///
/// Positions, offsets and alignment count from the start of the message
/// being written — the buffer's start, or wherever
/// [`Encoder::start_message`] last marked — so a message is encoded the same
/// alone, behind framing, or inside a compound.
#[derive(Debug, Default)]
pub struct Encoder {
    out: Vec<u8>,
    base: usize,
    failed: Option<SmbError>,
}

impl Encoder {
    /// An empty encoder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            out: Vec::new(),
            base: 0,
            failed: None,
        }
    }

    /// Count positions from here: the start of the next message's header.
    pub fn start_message(&mut self) {
        self.base = self.out.len();
    }

    /// Everything written, or the first failure.
    ///
    /// # Errors
    ///
    /// The first [`SmbError`] a write latched.
    pub fn finish(self) -> Result<Vec<u8>, SmbError> {
        match self.failed {
            None => Ok(self.out),
            Some(err) => Err(err),
        }
    }

    /// How many bytes have been written.
    #[must_use]
    pub fn position(&self) -> usize {
        self.out.len() - self.base
    }

    /// Latch `err` unless a failure already is.
    pub fn fail(&mut self, err: SmbError) {
        if self.failed.is_none() {
            self.failed = Some(err);
        }
    }

    /// Append `bytes`.
    pub fn bytes(&mut self, bytes: &[u8]) {
        if self.failed.is_some() {
            return;
        }
        if self.out.try_reserve(bytes.len()).is_err() {
            self.fail(SmbError::OutOfMemory);
            return;
        }
        self.out.extend_from_slice(bytes);
    }

    /// Append `len` zero bytes.
    pub fn zeros(&mut self, len: usize) {
        if self.failed.is_some() {
            return;
        }
        if self.out.try_reserve(len).is_err() {
            self.fail(SmbError::OutOfMemory);
            return;
        }
        self.out.resize(self.out.len() + len, 0);
    }

    /// Pad with zeros to a multiple of `alignment` from the start.
    pub fn align(&mut self, alignment: usize) {
        let position = self.position();
        self.zeros(position.next_multiple_of(alignment) - position);
    }

    /// A byte.
    pub fn u8(&mut self, value: u8) {
        self.bytes(&[value]);
    }

    /// A little-endian `u16`.
    pub fn u16(&mut self, value: u16) {
        self.bytes(&value.to_le_bytes());
    }

    /// A little-endian `u32`.
    pub fn u32(&mut self, value: u32) {
        self.bytes(&value.to_le_bytes());
    }

    /// A little-endian `u64`.
    pub fn u64(&mut self, value: u64) {
        self.bytes(&value.to_le_bytes());
    }

    /// Overwrite the `u16` at `at`, written earlier as a placeholder.
    pub fn patch_u16(&mut self, at: usize, value: u16) {
        self.patch(at, &value.to_le_bytes());
    }

    /// Overwrite the `u32` at `at`, written earlier as a placeholder.
    pub fn patch_u32(&mut self, at: usize, value: u32) {
        self.patch(at, &value.to_le_bytes());
    }

    fn patch(&mut self, at: usize, bytes: &[u8]) {
        if self.failed.is_some() {
            return;
        }
        let at = self.base + at;
        match self.out.get_mut(at..at + bytes.len()) {
            Some(slot) => slot.copy_from_slice(bytes),
            None => self.fail(SmbError::Malformed),
        }
    }

    /// A length or offset as the `u16` a field carries; a value that does not
    /// fit latches [`SmbError::TooLarge`] and writes zero.
    pub fn u16_of(&mut self, value: usize) -> u16 {
        u16::try_from(value).unwrap_or_else(|_| {
            self.fail(SmbError::TooLarge);
            0
        })
    }

    /// A length or offset as the `u32` a field carries; a value that does not
    /// fit latches [`SmbError::TooLarge`] and writes zero.
    pub fn u32_of(&mut self, value: usize) -> u32 {
        u32::try_from(value).unwrap_or_else(|_| {
            self.fail(SmbError::TooLarge);
            0
        })
    }
}

/// The seconds from FILETIME's epoch, 1601-01-01 UTC, to the Unix epoch.
const FILETIME_EPOCH_OFFSET: i64 = 11_644_473_600;

/// FILETIME's 100-nanosecond intervals in a second.
const FILETIME_TICKS: u64 = 10_000_000;

/// `time` as a FILETIME — 100-nanosecond intervals since 1601-01-01 UTC —
/// rounded down to that unit, for a timestamp whose finer precision the
/// protocol has no use for.
///
/// # Errors
///
/// [`SmbError::TimeOutOfRange`] before 1601 or past what 64 bits of
/// intervals reach.
pub fn filetime_floor(time: Time64) -> Result<u64, SmbError> {
    time.secs()
        .checked_add(FILETIME_EPOCH_OFFSET)
        .and_then(|secs| u64::try_from(secs).ok())
        .and_then(|secs| secs.checked_mul(FILETIME_TICKS))
        .and_then(|ticks| ticks.checked_add(u64::from(time.subsec_nanos() / 100)))
        .ok_or(SmbError::TimeOutOfRange)
}

/// `text` as UTF-16LE, the encoding SMB carries every name in.
pub fn utf16le(text: &str, enc: &mut Encoder) {
    for unit in text.encode_utf16() {
        enc.u16(unit);
    }
}

/// How many bytes `text` takes as UTF-16LE.
#[must_use]
pub fn utf16le_len(text: &str) -> usize {
    text.encode_utf16().count() * 2
}

/// UTF-16LE `bytes` as a string. An odd length or an unpaired surrogate is
/// [`SmbError::Malformed`]: a name that cannot round-trip is refused rather
/// than replaced.
///
/// # Errors
///
/// [`SmbError::Malformed`] as above; [`SmbError::OutOfMemory`] when the
/// string cannot grow.
pub fn from_utf16le(bytes: &[u8]) -> Result<String, SmbError> {
    let (units, rest) = bytes.as_chunks::<2>();
    if !rest.is_empty() {
        return Err(SmbError::Malformed);
    }
    let mut text = String::new();
    // A unit is at most three bytes of UTF-8, and a surrogate pair four.
    text.try_reserve(units.len() * 3)
        .map_err(|_| SmbError::OutOfMemory)?;
    for decoded in char::decode_utf16(units.iter().map(|unit| u16::from_le_bytes(*unit))) {
        text.push(decoded.map_err(|_| SmbError::Malformed)?);
    }
    Ok(text)
}
