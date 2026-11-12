//! RFC 1952 gzip: a member's header, its deflate body with the CRC-32 and
//! length its trailer is held to, and the walk across a stream of members.
//!
//! [`Header`] reads a header from bytes that arrive whole; [`Member`] carries
//! a body across calls, as a reader that resumes inside a member needs;
//! [`decompress`] walks a whole stream, handing its output to the caller as
//! it is produced. Like the rest of the crate it allocates nothing: the
//! member's state is a value the caller places.

use core::ops::Range;

use tairix_crc32::Crc32;

use crate::inflate::{self, Checkpoint, Inflater, Progress};

/// gzip's magic bytes.
pub const MAGIC: [u8; 2] = [0x1f, 0x8b];
/// The longest header read: a member whose extra field, name and comment run
/// past it is not one this system opens.
pub const HEADER_MAX: usize = 1 << 17;
/// The shortest member: a ten-byte header, an empty deflate block and the
/// trailer.
pub const MIN_LEN: usize = 20;
/// A member's trailer: its data's CRC-32, then its length modulo 2³², both
/// little-endian.
pub const TRAILER_LEN: usize = 8;

/// The deflate method, the only one RFC 1952 defines.
const DEFLATE: u8 = 8;
const FHCRC: u8 = 0x02;
const FEXTRA: u8 = 0x04;
const FNAME: u8 = 0x08;
const FCOMMENT: u8 = 0x10;
const RESERVED: u8 = 0xE0;

/// Why a stream was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Bytes where a member should start open none this system reads.
    Header,
    /// A member's deflate body does not decode.
    Body(inflate::Error),
    /// The stream ends inside a member.
    Truncated,
    /// A member's data does not match its trailer.
    Trailer,
    /// The caller's sink refused more output.
    Refused,
}

/// A member's header (RFC 1952 §2.3).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Header {
    /// Its length: where the compressed data starts.
    pub len: usize,
    /// The original file name it records, within the header's bytes, without
    /// its terminating NUL.
    pub name: Option<Range<usize>>,
    /// The modification time it records, in seconds since the Unix epoch;
    /// zero for none.
    pub mtime: u32,
}

impl Header {
    /// The header `bytes` open with, or `None` for bytes that open no member
    /// this system reads: no magic, a method other than deflate, a reserved
    /// flag set, a field running past the bytes or [`HEADER_MAX`], or a
    /// header checksum that does not hold.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..bytes.len().min(HEADER_MAX))?;
        if bytes.get(..2)? != MAGIC || *bytes.get(2)? != DEFLATE {
            return None;
        }
        let flags = *bytes.get(3)?;
        if flags & RESERVED != 0 {
            return None;
        }
        let mtime = u32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?);
        let mut at = 10usize;
        if flags & FEXTRA != 0 {
            let extra = usize::from(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
            at = at.checked_add(2 + extra)?;
        }
        let terminated = |at: usize| -> Option<(Range<usize>, usize)> {
            let field = bytes.get(at..)?;
            let nul = field.iter().position(|&byte| byte == 0)?;
            Some((at..at + nul, at + nul + 1))
        };
        let mut name = None;
        if flags & FNAME != 0 {
            let (field, next) = terminated(at)?;
            name = Some(field);
            at = next;
        }
        if flags & FCOMMENT != 0 {
            at = terminated(at)?.1;
        }
        if flags & FHCRC != 0 {
            let stored = u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?);
            if u32::from(stored) != tairix_crc32::checksum(bytes.get(..at)?) & 0xFFFF {
                return None;
            }
            at += 2;
        }
        (at <= bytes.len()).then_some(Self {
            len: at,
            name,
            mtime,
        })
    }
}

/// A member's body being decoded, and the checksum and length its trailer
/// is held to. Around 34 KiB — see [`Inflater`] on where to keep it.
pub struct Member {
    inflater: Inflater,
    crc: Crc32,
    len: u64,
    /// The body was decoded from its start, so its trailer can be checked.
    checked: bool,
}

impl Default for Member {
    fn default() -> Self {
        Self {
            inflater: Inflater::new(),
            crc: Crc32::new(),
            len: 0,
            checked: true,
        }
    }
}

impl Member {
    /// A fresh member.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a member's body at its first byte.
    pub fn begin(&mut self) {
        self.inflater.reset();
        self.crc = Crc32::new();
        self.len = 0;
        self.checked = true;
    }

    /// Resume a member's body at the block boundary `checkpoint` records;
    /// what came before is unseen, so its trailer cannot be checked.
    pub fn resume(&mut self, checkpoint: &Checkpoint) {
        self.inflater.resume(checkpoint);
        self.crc = Crc32::new();
        self.len = 0;
        self.checked = false;
    }

    /// Whether the body has ended.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.inflater.is_finished()
    }

    /// Whether the body stands at a deflate block's start.
    #[must_use]
    pub fn at_block_start(&self) -> bool {
        self.inflater.at_block_start()
    }

    /// Record where the body stands, for [`Member::resume`].
    pub fn checkpoint(&self, checkpoint: &mut Checkpoint) -> bool {
        self.inflater.checkpoint(checkpoint)
    }

    /// Decode as much of `src` into `dst` as both allow.
    ///
    /// # Errors
    ///
    /// The deflate body's own refusal.
    pub fn inflate(&mut self, src: &[u8], dst: &mut [u8]) -> Result<Progress, inflate::Error> {
        let progress = self.inflater.inflate(src, dst)?;
        self.count(&dst[..progress.produced]);
        Ok(progress)
    }

    /// Decode as [`Member::inflate`] does, stopping at the next block
    /// boundary.
    ///
    /// # Errors
    ///
    /// The deflate body's own refusal.
    pub fn inflate_by_block(
        &mut self,
        src: &[u8],
        dst: &mut [u8],
    ) -> Result<Progress, inflate::Error> {
        let progress = self.inflater.inflate_by_block(src, dst)?;
        self.count(&dst[..progress.produced]);
        Ok(progress)
    }

    fn count(&mut self, produced: &[u8]) {
        self.crc.update(produced);
        self.len += produced.len() as u64;
    }

    /// Hold the ended body to its trailer; a resumed body passes unchecked.
    #[must_use]
    pub fn verify(&mut self, trailer: &[u8; TRAILER_LEN]) -> bool {
        let crc = core::mem::take(&mut self.crc).finish();
        let len = self.len.to_le_bytes();
        !self.checked || (trailer[..4] == crc.to_le_bytes() && trailer[4..] == len[..4])
    }
}

/// Decode the whole stream `src`, member after member, handing each piece of
/// output to `sink` as `scratch` fills; trailing zero padding ends it. The
/// sink returns whether to go on. `scratch` must not be empty: a member with
/// data in it could make no progress into one.
///
/// # Errors
///
/// [`Error`]: a refused header, body or trailer, a stream ending inside a
/// member, or the sink refusing more.
pub fn decompress(
    src: &[u8],
    member: &mut Member,
    scratch: &mut [u8],
    mut sink: impl FnMut(&[u8]) -> bool,
) -> Result<(), Error> {
    let mut at = 0;
    loop {
        let rest = &src[at..];
        if at > 0 && rest.first() == Some(&0) && rest.iter().all(|&byte| byte == 0) {
            return Ok(());
        }
        let header = Header::parse(rest).ok_or(Error::Header)?;
        at += header.len;
        member.begin();
        while !member.is_finished() {
            let progress = member.inflate(&src[at..], scratch).map_err(Error::Body)?;
            at += progress.consumed;
            if progress.produced > 0 && !sink(&scratch[..progress.produced]) {
                return Err(Error::Refused);
            }
            if progress.consumed == 0 && progress.produced == 0 && !member.is_finished() {
                return Err(Error::Truncated);
            }
        }
        let trailer: &[u8; TRAILER_LEN] = src
            .get(at..at + TRAILER_LEN)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(Error::Truncated)?;
        if !member.verify(trailer) {
            return Err(Error::Trailer);
        }
        at += TRAILER_LEN;
        if at == src.len() {
            return Ok(());
        }
    }
}

#[cfg(test)]
#[path = "gzip_tests.rs"]
mod tests;
