//! RISC OS's Squash: its file, a `SQSH` header before a stream, and its
//! stream, Unix `compress -b 12` with compress's three-byte header, coded as
//! the Squash module codes it — what it writes, byte for byte, and what it
//! reads.
//!
//! The module's two SWIs code a stream a call at a time, each restartable
//! from its workspace alone, and so do [`compress`] and [`decompress`]: the
//! workspace holds the coder's whole state, so a call given that workspace
//! carries on whatever stream it holds. A stream is placed in a workspace by
//! [`start_compression`] or [`start_decompression`]. Workspace a stranger
//! wrote is checked before it is trusted, and a call over it refuses rather
//! than misbehaves.
//!
//! The module compresses two ways, which write different streams of the same
//! data: when one call holds the whole input and room for anything it could
//! make, its table is cleared as soon as it is full; otherwise not until
//! later. [`Clearing`] names them.

use crate::lzw::{self, Decoding, Dialect, Encoding, DECODING_STATE, ENCODING_STATE};

pub use crate::lzw::{Place, Progress, Status};

/// The RISC OS file type of a Squash file.
pub const FILE_TYPE: u16 = 0xFCA;

/// The bytes a Squash file opens with.
pub const FILE_MAGIC: [u8; 4] = *b"SQSH";

/// The bytes of a Squash file's header.
pub const FILE_HEADER_LEN: usize = 20;

/// The bytes a Squash stream's header takes.
const STREAM_HEADER: u32 = 3;

/// The header a Squash stream opens with: compress's magic, then its flags —
/// block mode, codes to twelve bits.
pub const STREAM_MAGIC: [u8; STREAM_HEADER as usize] = [0x1F, 0x9D, 0x8C];

/// The widest code a Squash stream holds.
const MAX_BITS: u8 = 12;

/// The input a [`decompress`] call must be given at once, where the stream
/// has that much left, to be sure of taking some: a group of its widest
/// codes.
pub const GROUP_MOST: usize = MAX_BITS as usize;

/// The workspace a compressing stream needs, as the module asks for it.
pub const COMPRESS_WORK: usize = 0x7C00;

/// The workspace a decompressing stream needs, as the module asks for it.
pub const DECOMPRESS_WORK: usize = 0x4400;

/// What opens a workspace a compressing stream stands in.
const COMPRESSING: [u8; 4] = *b"SqzC";
/// What opens a workspace a decompressing stream stands in.
const DECOMPRESSING: [u8; 4] = *b"SqzD";

/// A Squash file's header: what the file was before it was squashed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FileHeader {
    /// Its length.
    pub length: u32,
    /// Its load address, which typically holds its filetype and the high
    /// byte of its date stamp.
    pub load: u32,
    /// Its execution address, typically the rest of its date stamp.
    pub exec: u32,
}

impl FileHeader {
    /// The header `bytes` open with, or `None` where they do not open a
    /// Squash file. The reserved word is ignored, as one a writer should
    /// leave zero.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let (magic, fields) = bytes.split_first_chunk::<4>()?;
        let fields = fields.first_chunk::<{ FILE_HEADER_LEN - 4 }>()?;
        if *magic != FILE_MAGIC {
            return None;
        }
        let word = |at: usize| {
            u32::from_le_bytes([fields[at], fields[at + 1], fields[at + 2], fields[at + 3]])
        };
        Some(Self {
            length: word(0),
            load: word(4),
            exec: word(8),
        })
    }

    /// The header as a file opens with it, its reserved word zero.
    #[must_use]
    pub fn to_bytes(self) -> [u8; FILE_HEADER_LEN] {
        let mut bytes = [0; FILE_HEADER_LEN];
        bytes[..4].copy_from_slice(&FILE_MAGIC);
        for (at, word) in [(4, self.length), (8, self.load), (12, self.exec)] {
            bytes[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
        bytes
    }
}

/// When a compressing stream clears its table: which of the module's two
/// ways it is compressed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Clearing {
    /// The module's when one call holds the whole input and room for
    /// anything it could make: as soon as the table is full, its last code
    /// never assigned.
    AtOnce,
    /// Its otherwise: once the table is full, every 32 KiB of input.
    Periodically,
}

impl Clearing {
    /// How the module compresses a new stream whose first call gives it
    /// `input` bytes and `room` for output, `more` saying whether input
    /// follows: at once only where that call holds all of it and room for
    /// half as much again and the header, reckoned in its 32-bit words.
    #[must_use]
    pub const fn for_first_call(input: u32, room: u32, more: bool) -> Self {
        let whole = (input.wrapping_mul(3) >> 1).wrapping_add(STREAM_HEADER);
        if !more && room >= whole {
            Self::AtOnce
        } else {
            Self::Periodically
        }
    }
}

/// The most output the module says `input` bytes can compress to: half as
/// much again and twelve bytes, reckoned in its 32-bit words.
#[must_use]
pub const fn max_output(input: u32) -> u32 {
    (input.wrapping_mul(3) >> 1).wrapping_add(12)
}

/// Why a call refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// The input is not a Squash stream: its header is not one, or its codes
    /// code what no table can.
    Input,
    /// The workspace is short of the size the coder needs, or holds no
    /// stream a call of this kind left there.
    Workspace,
}

impl From<lzw::Error> for Error {
    fn from(err: lzw::Error) -> Self {
        match err {
            lzw::Error::Corrupt => Self::Input,
            lzw::Error::Workspace => Self::Workspace,
        }
    }
}

/// A workspace as a coder stands in it: its mark, whether the stream's
/// header is written or read, the coder's own state, then its tables.
struct Parts<'w, const CODER: usize> {
    mark: &'w mut [u8; 4],
    headed: &'w mut u8,
    coder: &'w mut [u8; CODER],
    tables: &'w mut [u8],
}

impl<'w, const CODER: usize> Parts<'w, CODER> {
    /// The first `len` bytes of `work`, as a coder stands in them.
    fn of(work: &'w mut [u8], len: usize) -> Result<Self, Error> {
        let work = work.get_mut(..len).ok_or(Error::Workspace)?;
        let (mark, work) = work.split_first_chunk_mut::<4>().ok_or(Error::Workspace)?;
        let (headed, work) = work.split_first_mut().ok_or(Error::Workspace)?;
        let (coder, tables) = work.split_first_chunk_mut().ok_or(Error::Workspace)?;
        Ok(Self {
            mark,
            headed,
            coder,
            tables,
        })
    }

    /// Whether the stream's header is done, where the workspace holds a
    /// stream marked `mark`.
    fn headed(&self, mark: [u8; 4]) -> Result<bool, Error> {
        match (*self.mark == mark, *self.headed) {
            (true, 0) => Ok(false),
            (true, 1) => Ok(true),
            _ => Err(Error::Workspace),
        }
    }

    /// Mark the workspace as holding a stream marked `mark`, with whether its
    /// header is done.
    fn mark(&mut self, mark: [u8; 4], headed: bool) {
        *self.mark = mark;
        *self.headed = u8::from(headed);
    }
}

/// Place a new compressing stream in `work`, its table cleared as
/// `clearing` says.
///
/// # Errors
///
/// [`Error::Workspace`] for workspace short of [`COMPRESS_WORK`] bytes.
pub fn start_compression(work: &mut [u8], clearing: Clearing) -> Result<(), Error> {
    let mut parts = Parts::<ENCODING_STATE>::of(work, COMPRESS_WORK)?;
    let clearing = match clearing {
        Clearing::AtOnce => lzw::Clearing::AtLast,
        Clearing::Periodically => lzw::Clearing::Periodic,
    };
    let coding = Encoding::new(clearing, MAX_BITS, parts.tables).ok_or(Error::Workspace)?;
    parts.mark(COMPRESSING, false);
    coding.save(parts.coder);
    Ok(())
}

/// Compress `input` into `out` as `Squash_Compress` does, carrying on the
/// stream `work` holds, `more` saying whether input follows this, each place
/// its table is cleared told to `cleared`; with none to follow, the stream
/// is ended once `input` is taken. The stream's header is written as its
/// first byte of input is taken, or as it ends with none taken, so that an
/// empty input is the header alone, which [`decompress`] reads as nothing.
/// Output is written a group of codes at a time, a group with no room held
/// for the next call.
///
/// # Errors
///
/// [`Error::Workspace`] for workspace short of [`COMPRESS_WORK`] bytes, or
/// that holds no compressing stream.
pub fn compress(
    work: &mut [u8],
    input: &[u8],
    more: bool,
    out: &mut [u8],
    cleared: &mut dyn FnMut(Place),
) -> Result<Progress, Error> {
    let mut parts = Parts::<ENCODING_STATE>::of(work, COMPRESS_WORK)?;
    let mut headed = parts.headed(COMPRESSING)?;
    let mut coding = Encoding::restore(parts.coder, MAX_BITS, parts.tables.len())?;
    let mut written = 0;
    if !headed && (!input.is_empty() || !more) {
        let Some(head) = out.get_mut(..STREAM_MAGIC.len()) else {
            return Ok(Progress {
                status: Status::Output,
                consumed: 0,
                produced: 0,
            });
        };
        head.copy_from_slice(&STREAM_MAGIC);
        written = STREAM_MAGIC.len();
        headed = true;
    }
    let mut progress = coding.encode(
        parts.tables,
        input,
        more,
        &mut out[written..],
        &mut |place| {
            cleared(Place {
                packed: place.packed + written,
                ..place
            });
        },
    )?;
    progress.produced += written;
    parts.mark(COMPRESSING, headed);
    coding.save(parts.coder);
    Ok(progress)
}

/// Place a new decompressing stream in `work`, its header yet to be read.
///
/// # Errors
///
/// [`Error::Workspace`] for workspace short of [`DECOMPRESS_WORK`] bytes.
pub fn start_decompression(work: &mut [u8]) -> Result<(), Error> {
    place_decompression(work, false)
}

/// Place in `work` a decompressing stream standing at a [`Place`]: past its
/// header, its table empty, so [`decompress`] decodes the stream on from
/// that place.
///
/// # Errors
///
/// [`Error::Workspace`] for workspace short of [`DECOMPRESS_WORK`] bytes.
pub fn start_decompression_at_place(work: &mut [u8]) -> Result<(), Error> {
    place_decompression(work, true)
}

fn place_decompression(work: &mut [u8], headed: bool) -> Result<(), Error> {
    let mut parts = Parts::<DECODING_STATE>::of(work, DECOMPRESS_WORK)?;
    let coding = Decoding::new(Dialect::Squash, MAX_BITS).ok_or(Error::Workspace)?;
    parts.mark(DECOMPRESSING, headed);
    coding.save(parts.coder);
    Ok(())
}

/// Decompress `input` into `out` as `Squash_Decompress` does, carrying on
/// the stream `work` holds, `more` saying whether input follows this, each
/// place its table is cleared told to `cleared`. The stream's header is read
/// first: only its third byte, the flags, is held to what Squash writes.
/// Input is taken a group of codes at a time while more follows, the final
/// short group only at the end; a string with no room left is held for the
/// next call.
///
/// # Errors
///
/// [`Error::Input`] for input that opens no Squash stream, or ends before its
/// header does, or codes what no table can; [`Error::Workspace`] for
/// workspace short of [`DECOMPRESS_WORK`] bytes, or that holds no
/// decompressing stream.
pub fn decompress(
    work: &mut [u8],
    input: &[u8],
    more: bool,
    out: &mut [u8],
    cleared: &mut dyn FnMut(Place),
) -> Result<Progress, Error> {
    let mut parts = Parts::<DECODING_STATE>::of(work, DECOMPRESS_WORK)?;
    let mut headed = parts.headed(DECOMPRESSING)?;
    let mut coding = Decoding::restore(parts.coder, Dialect::Squash, MAX_BITS)?;
    let mut taken = 0;
    if !headed {
        match input.get(..STREAM_MAGIC.len()) {
            None if more => {
                return Ok(Progress {
                    status: Status::Input,
                    consumed: 0,
                    produced: 0,
                });
            }
            Some(&[_, _, flags]) if flags == STREAM_MAGIC[2] => {}
            _ => return Err(Error::Input),
        }
        taken = STREAM_MAGIC.len();
        headed = true;
    }
    let mut progress = coding.decode(parts.tables, &input[taken..], more, out, &mut |place| {
        cleared(Place {
            packed: place.packed + taken,
            ..place
        });
    })?;
    progress.consumed += taken;
    parts.mark(DECOMPRESSING, headed);
    coding.save(parts.coder);
    Ok(progress)
}

#[cfg(test)]
#[path = "squash_tests.rs"]
mod tests;
