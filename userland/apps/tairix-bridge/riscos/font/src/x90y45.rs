//! A font's `x90y45` file, the bitmap format before outlines, read as the PRM
//! lays it out: an index of the sizes it holds, each a block of up to 256
//! characters at four bits a pixel, its rows from the bottom up. A file
//! shorter than 256 bytes names the `f9999x9999` file whose bitmaps are the
//! master ones. The file is the guest's: every offset and length is
//! checked, and what does not hold together is refused. Its bytes are
//! zeroed when let go.

use alloc::vec::Vec;
use core::mem::size_of;

use crate::glyphs::{Bitmap, Refused};
use crate::outlines::{Malformed, Undecoded, REDIRECT_BELOW};

/// An index entry's bytes, and its fields.
const ENTRY: usize = 16;
const POINTS: usize = 0;
const BPP: usize = 1;
const DOTS_ACROSS: usize = 2;
const DOTS_UP: usize = 3;
const DATA_AT: usize = 8;
const DATA_SIZE: usize = 12;
/// The only depth the format holds.
const FOUR_BITS: u8 = 4;
/// A block's header: its sizes, resolutions and box, then each character's
/// offset from the table.
const BLOCK_TABLE: usize = 20;
const CHARACTERS: usize = 256;
/// A block holds no more than this.
const BLOCK_MOST: usize = 64 * 1024;

/// One size an `x90y45` file holds: its points, not in sixteenths, its dots
/// an inch across and up, and where its block lies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Size {
    pub(crate) points: u8,
    pub(crate) resolution: (u8, u8),
    block: (usize, usize),
}

/// What an `x90y45` file is.
#[derive(Debug)]
pub(crate) enum File {
    /// Bitmaps at the sizes it lists.
    Sizes(Sizes),
    /// The name of the `f9999x9999` file holding the master bitmaps, to its
    /// control character.
    Master(Vec<u8>),
}

impl File {
    /// The file `bytes`.
    pub(crate) fn read(bytes: Vec<u8>) -> Result<Self, Malformed> {
        if bytes.len() < REDIRECT_BELOW {
            let end = bytes.iter().position(|&b| b < b' ').unwrap_or(bytes.len());
            let mut name = bytes;
            name.truncate(end);
            return Ok(Self::Master(name));
        }
        Sizes::read(bytes).map(Self::Sizes)
    }
}

/// An `x90y45` file's sizes.
#[derive(Debug)]
pub(crate) struct Sizes {
    bytes: Vec<u8>,
    pub(crate) sizes: Vec<Size>,
}

impl Sizes {
    fn read(bytes: Vec<u8>) -> Result<Self, Malformed> {
        let mut sizes = Vec::new();
        let mut at = 0;
        loop {
            let points = *bytes.get(at + POINTS).ok_or(Malformed)?;
            if points == 0 {
                break;
            }
            let entry = bytes.get(at..at + ENTRY).ok_or(Malformed)?;
            if entry[BPP] != FOUR_BITS {
                return Err(Malformed);
            }
            let word = |field: usize| {
                usize::try_from(u32::from_le_bytes([
                    entry[field],
                    entry[field + 1],
                    entry[field + 2],
                    entry[field + 3],
                ]))
                .map_err(|_| Malformed)
            };
            let (start, length) = (word(DATA_AT)?, word(DATA_SIZE)?);
            let end = start.checked_add(length).ok_or(Malformed)?;
            if !(BLOCK_TABLE + 2 * CHARACTERS..=BLOCK_MOST).contains(&length)
                || end > bytes.len()
                || start % 4 != 0
            {
                return Err(Malformed);
            }
            sizes.try_reserve(1).map_err(|_| Malformed)?;
            sizes.push(Size {
                points,
                resolution: (entry[DOTS_ACROSS], entry[DOTS_UP]),
                block: (start, end),
            });
            at += ENTRY;
        }
        Ok(Self { bytes, sizes })
    }

    /// Roughly what it holds in memory.
    pub(crate) fn bytes(&self) -> usize {
        size_of::<Self>() + self.bytes.len() + self.sizes.len() * size_of::<Size>()
    }

    /// Character `code`'s pixels at `size`, as levels; none for a character
    /// the size does not define, one with no pixels, or one whose data does
    /// not hold together.
    pub(crate) fn glyph(&self, size: &Size, code: u32) -> Result<Option<Bitmap>, Refused> {
        match self.decoded(size, code) {
            Ok(bitmap) => Ok(bitmap),
            Err(Undecoded::Malformed) => Ok(None),
            Err(Undecoded::Refused) => Err(Refused),
        }
    }

    fn decoded(&self, size: &Size, code: u32) -> Result<Option<Bitmap>, Undecoded> {
        let Ok(code) = usize::try_from(code) else {
            return Ok(None);
        };
        if code >= CHARACTERS {
            return Ok(None);
        }
        let block = &self.bytes[size.block.0..size.block.1];
        let offset_at = BLOCK_TABLE + 2 * code;
        let offset = usize::from(u16::from_le_bytes([block[offset_at], block[offset_at + 1]]));
        if offset == 0 {
            return Ok(None);
        }
        let at = BLOCK_TABLE.checked_add(offset).ok_or(Malformed)?;
        let header = block.get(at..at + 4).ok_or(Malformed)?;
        let [x0, y0, width, height] = [header[0], header[1], header[2], header[3]];
        let (width, height) = (usize::from(width), usize::from(height));
        if width == 0 || height == 0 {
            return Ok(None);
        }
        let pixels = width * height;
        let data = block
            .get(at + 4..at + 4 + pixels.div_ceil(2))
            .ok_or(Malformed)?;
        let mut levels = Vec::new();
        levels
            .try_reserve_exact(pixels)
            .map_err(|_| Undecoded::Refused)?;
        levels.resize(pixels, 0);
        for i in 0..pixels {
            let (row, column) = (height - 1 - i / width, i % width);
            let byte = data[i / 2];
            levels[row * width + column] = if i % 2 == 0 { byte & 0xF } else { byte >> 4 };
        }
        let x0 = i32::from(x0.cast_signed());
        let y0 = i32::from(y0.cast_signed());
        Ok(Some(Bitmap {
            left: x0,
            top: y0 + i32::try_from(height).map_err(|_| Malformed)? - 1,
            width,
            levels,
        }))
    }
}

impl Drop for Sizes {
    fn drop(&mut self) {
        self.bytes.fill(0);
    }
}

#[cfg(test)]
mod tests;
