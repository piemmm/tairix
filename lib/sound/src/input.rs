//! The bytes a decoder reads, at offsets of its choosing.

use crate::DecodeError;

/// Why an input did not answer a read.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum InputError {
    /// The bytes are not at hand yet. The call that met this changed nothing,
    /// so it may be made again once they are.
    Unavailable,
    /// The input failed, and asking again will not help.
    Failed,
}

/// A file a decoder reads.
pub trait SoundInput {
    /// The file's length in bytes.
    fn len(&self) -> u64;

    /// Whether the file holds no bytes.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Fill `buf` with the bytes at `offset`, answering how many: fewer only
    /// where the file ends.
    ///
    /// # Errors
    ///
    /// [`InputError`] when the bytes cannot be read now or at all.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, InputError>;
}

impl SoundInput for &[u8] {
    fn len(&self) -> u64 {
        u64::try_from(<[u8]>::len(self)).unwrap_or(u64::MAX)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, InputError> {
        let Some(rest) = usize::try_from(offset).ok().and_then(|at| self.get(at..)) else {
            return Ok(0);
        };
        let count = rest.len().min(buf.len());
        buf[..count].copy_from_slice(&rest[..count]);
        Ok(count)
    }
}

impl From<InputError> for DecodeError {
    fn from(err: InputError) -> Self {
        match err {
            InputError::Unavailable => Self::InputUnavailable,
            InputError::Failed => Self::InputFailed,
        }
    }
}

/// Most bytes one read asks an input for: a longer read is made in pieces,
/// so an input serving its file from a bounded cache is never asked for more
/// of it at once.
pub const MAX_READ: usize = 64 * 1024;

/// Fill `buf` from `offset`, answering how many bytes the file held there.
pub(crate) fn read(
    input: &mut (impl SoundInput + ?Sized),
    offset: u64,
    buf: &mut [u8],
) -> Result<usize, DecodeError> {
    let mut held = 0;
    for piece in buf.chunks_mut(MAX_READ) {
        let at = offset
            .checked_add(u64::try_from(held).map_err(|_| DecodeError::InputFailed)?)
            .ok_or(DecodeError::InputFailed)?;
        let count = input.read_at(at, piece)?;
        held += count;
        if count < piece.len() {
            break;
        }
    }
    Ok(held)
}

/// Fill `buf` from `offset` whole, or refuse with `short` where the file
/// ends first.
pub(crate) fn read_exact(
    input: &mut (impl SoundInput + ?Sized),
    offset: u64,
    buf: &mut [u8],
    short: DecodeError,
) -> Result<(), DecodeError> {
    if read(input, offset, buf)? == buf.len() {
        Ok(())
    } else {
        Err(short)
    }
}

/// A run of bytes a decoder reads at offsets within it: a range of the file,
/// or a container's packet laid across the file.
pub(crate) trait Region {
    /// Bytes the region holds.
    fn len(&self) -> u64;

    /// Fill `buf` from `at` within the region whole, or refuse with `short`
    /// where the region ends first.
    fn read_exact(
        &mut self,
        at: u64,
        buf: &mut [u8],
        short: DecodeError,
    ) -> Result<(), DecodeError>;
}

/// `len` bytes of the file from `start`.
pub(crate) struct Span<'i, I: SoundInput + ?Sized> {
    pub(crate) input: &'i mut I,
    pub(crate) start: u64,
    pub(crate) len: u64,
}

impl<I: SoundInput + ?Sized> Region for Span<'_, I> {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_exact(
        &mut self,
        at: u64,
        buf: &mut [u8],
        short: DecodeError,
    ) -> Result<(), DecodeError> {
        let wanted = u64::try_from(buf.len()).map_err(|_| short)?;
        if at.checked_add(wanted).is_none_or(|end| end > self.len) {
            return Err(short);
        }
        read_exact(self.input, self.start + at, buf, short)
    }
}

/// A cursor through a region, reading its fields in order.
pub(crate) struct Fields<'r, R: Region> {
    region: &'r mut R,
    at: u64,
    short: DecodeError,
}

impl<'r, R: Region> Fields<'r, R> {
    /// Read `region` from its start, refusing with `short` past its end.
    pub(crate) fn new(region: &'r mut R, short: DecodeError) -> Self {
        Self {
            region,
            at: 0,
            short,
        }
    }

    /// Bytes past the cursor.
    pub(crate) fn remaining(&self) -> u64 {
        self.region.len() - self.at
    }

    /// Where the cursor is, from the region's start.
    pub(crate) const fn position(&self) -> u64 {
        self.at
    }

    /// Fill `buf` from the cursor on.
    pub(crate) fn bytes(&mut self, buf: &mut [u8]) -> Result<(), DecodeError> {
        self.region.read_exact(self.at, buf, self.short)?;
        self.at += u64::try_from(buf.len()).map_err(|_| self.short)?;
        Ok(())
    }

    /// The next `N` bytes.
    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let mut bytes = [0u8; N];
        self.bytes(&mut bytes)?;
        Ok(bytes)
    }

    /// Move the cursor `count` bytes on, refusing a move past the end.
    pub(crate) fn skip(&mut self, count: u64) -> Result<(), DecodeError> {
        if count > self.remaining() {
            return Err(self.short);
        }
        self.at += count;
        Ok(())
    }

    /// The error a field past the end is refused with.
    pub(crate) const fn short(&self) -> DecodeError {
        self.short
    }
}
