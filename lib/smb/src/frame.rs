//! The Direct TCP transport (MS-SMB2 §2.1): each frame is a zero byte, a
//! 24-bit big-endian length, and that many bytes — one message, a compound of
//! several, or a transform carrying either.

use alloc::vec::Vec;
use core::ops::Range;

use crate::header::{HEADER_LEN, NEXT_COMMAND_RANGE};
use crate::wire::owned;
use crate::SmbError;

/// The largest frame this engine sends or accepts: a fixed bound on what a
/// server can make a client buffer, and the sandbox frame a worker moves.
pub const MAX_FRAME: usize = 8 << 20;

/// The most messages one compound may carry.
pub const MAX_COMPOUND: usize = 32;

/// The length of the prefix in front of every frame.
pub const PREFIX_LEN: usize = 4;

/// The prefix in front of a frame of `len` bytes.
///
/// # Errors
///
/// [`SmbError::TooLarge`] past [`MAX_FRAME`].
pub fn prefix(len: usize) -> Result<[u8; PREFIX_LEN], SmbError> {
    if len > MAX_FRAME {
        return Err(SmbError::TooLarge);
    }
    let len = u32::try_from(len).map_err(|_| SmbError::TooLarge)?;
    Ok(len.to_be_bytes())
}

/// One whole frame, owned: the bytes from `at` to the end of `buf`.
#[derive(Debug)]
pub struct Frame {
    buf: Vec<u8>,
    at: usize,
}

impl Frame {
    /// The buffer and the offset the frame starts at; it runs to the end.
    #[must_use]
    pub fn into_parts(self) -> (Vec<u8>, usize) {
        (self.buf, self.at)
    }
}

/// Collects a byte stream into whole frames.
///
/// A frame that ends the bytes buffered is handed over in the buffer it
/// arrived in, so a large one is never copied; one followed by more bytes
/// costs a copy of whichever of the two is shorter.
#[derive(Debug, Default)]
pub struct Framer {
    buf: Vec<u8>,
    start: usize,
}

impl Framer {
    /// A framer holding nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: Vec::new(),
            start: 0,
        }
    }

    /// Take `bytes` the stream delivered.
    ///
    /// # Errors
    ///
    /// [`SmbError::OutOfMemory`] when the buffer cannot grow.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), SmbError> {
        if self.start > 0 && self.start >= self.buf.len() / 2 {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        self.buf
            .try_reserve(bytes.len())
            .map_err(|_| SmbError::OutOfMemory)?;
        self.buf.extend_from_slice(bytes);
        Ok(())
    }

    /// The next whole frame, without its prefix, or `None` until one has
    /// arrived.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] for a prefix whose first byte is not zero,
    /// [`SmbError::TooLarge`] for a frame past [`MAX_FRAME`] — either ends
    /// the stream, which can no longer be framed — and
    /// [`SmbError::OutOfMemory`] when a frame cannot be given its own buffer.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, SmbError> {
        let pending = &self.buf[self.start..];
        let Some(prefix) = pending.get(..PREFIX_LEN) else {
            return Ok(None);
        };
        if prefix[0] != 0 {
            return Err(SmbError::Malformed);
        }
        let len = usize::try_from(u32::from_be_bytes([0, prefix[1], prefix[2], prefix[3]]))
            .map_err(|_| SmbError::TooLarge)?;
        if len > MAX_FRAME {
            return Err(SmbError::TooLarge);
        }
        let missing = (PREFIX_LEN + len).saturating_sub(pending.len());
        if missing > 0 {
            // Room for the rest now, so a large frame is not regrown on the
            // way in.
            self.buf
                .try_reserve(missing)
                .map_err(|_| SmbError::OutOfMemory)?;
            return Ok(None);
        }
        let at = self.start + PREFIX_LEN;
        let end = at + len;
        let tail = &self.buf[end..];
        if tail.len() <= len {
            let rest = owned(tail)?;
            let mut buf = core::mem::replace(&mut self.buf, rest);
            buf.truncate(end);
            self.start = 0;
            Ok(Some(Frame { buf, at }))
        } else {
            let buf = owned(&self.buf[at..end])?;
            self.start = end;
            Ok(Some(Frame { buf, at: 0 }))
        }
    }
}

/// Where each message of one plaintext frame lies, split where each
/// `NextCommand` says: eight-byte aligned, past the header, inside the frame,
/// and zero on the last.
///
/// # Errors
///
/// [`SmbError::Malformed`] for an offset breaking those rules,
/// [`SmbError::TooLarge`] past [`MAX_COMPOUND`] messages.
pub fn messages(frame: &[u8]) -> Result<Vec<Range<usize>>, SmbError> {
    let mut out = Vec::new();
    let mut start = 0;
    loop {
        if out.len() == MAX_COMPOUND {
            return Err(SmbError::TooLarge);
        }
        let rest = &frame[start..];
        let next = rest
            .get(NEXT_COMMAND_RANGE)
            .map(|field| u32::from_le_bytes([field[0], field[1], field[2], field[3]]))
            .ok_or(SmbError::Malformed)?;
        let next = usize::try_from(next).map_err(|_| SmbError::Malformed)?;
        out.try_reserve(1).map_err(|_| SmbError::OutOfMemory)?;
        if next == 0 {
            out.push(start..frame.len());
            return Ok(out);
        }
        if next < HEADER_LEN || next % 8 != 0 || next >= rest.len() {
            return Err(SmbError::Malformed);
        }
        out.push(start..start + next);
        start += next;
    }
}

#[cfg(test)]
#[path = "frame_tests.rs"]
mod tests;
