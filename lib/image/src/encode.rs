//! Writing pictures: PNG, baseline JPEG, and RISC OS sprite areas.
//!
//! Every encoder reads its picture a row at a time through
//! [`PictureSource`], so a caller holding pixels in tiles never flattens
//! them, and every one validates what it reads — an index past the palette,
//! a mask its format cannot state — refusing rather than writing a file that
//! says something other than the picture.

use alloc::vec::Vec;
use core::fmt;

use crate::picture::{IndexDepth, PictureSource, Rgba8};
use crate::sprite_encode::SpriteInput;
use crate::{jpeg_encode, png_encode, sprite_encode};

/// Why a picture could not be written.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EncodeError {
    /// A buffer the encoder needs was refused by the allocator.
    OutOfMemory,
    /// The picture is larger than the format can describe.
    TooLarge,
    /// A pixel names a colour past the palette's end.
    IndexOutOfRange,
    /// The palette is empty or longer than its depth indexes.
    InvalidPalette,
    /// A JPEG quality outside `1..=100`.
    InvalidQuality,
    /// A sprite's pixels are not the kind its mode lays out.
    SpriteLayoutMismatch,
    /// A sprite's palette does not state the colours its pixels are shown
    /// in.
    SpritePaletteMismatch,
    /// A sprite palette entry is translucent, which a sprite palette cannot
    /// state.
    SpritePaletteAlpha,
    /// A sprite's mask is partly transparent but its mode holds only a
    /// binary one.
    SpriteMaskNotBinary,
    /// A sprite's pixels are transparent but it has no mask or alpha channel
    /// to say so.
    SpriteAlphaUnrepresentable,
    /// A sprite kept as its bytes is shorter than a control block or not a
    /// whole number of words.
    SpriteOpaqueMalformed,
    /// A sprite area with no sprites.
    SpriteAreaEmpty,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OutOfMemory => "there is not enough memory to write the picture",
            Self::TooLarge => "the picture is larger than the format can describe",
            Self::IndexOutOfRange => "a pixel names a colour past the palette's end",
            Self::InvalidPalette => "the palette is empty or longer than its depth allows",
            Self::InvalidQuality => "a JPEG quality must be between 1 and 100",
            Self::SpriteLayoutMismatch => "a sprite's pixels are not the kind its mode holds",
            Self::SpritePaletteMismatch => "a sprite's palette does not match its colours",
            Self::SpritePaletteAlpha => "a sprite palette cannot hold a translucent colour",
            Self::SpriteMaskNotBinary => {
                "a sprite's mask is partly transparent but its mode holds only on or off"
            }
            Self::SpriteAlphaUnrepresentable => {
                "a sprite's pixels are transparent but it has no mask to say so"
            }
            Self::SpriteOpaqueMalformed => "a sprite kept as it was read is malformed",
            Self::SpriteAreaEmpty => "a sprite area needs at least one sprite",
        })
    }
}

/// How a JPEG is written.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct JpegOptions {
    quality: u8,
    background: [u8; 3],
}

impl JpegOptions {
    /// The quality the desktop writes at unless asked otherwise.
    pub const DEFAULT_QUALITY: u8 = 90;

    /// Write at `quality`, from 1 (smallest) to 100 (finest), compositing
    /// any transparency over `background`, since JPEG has none.
    ///
    /// # Errors
    ///
    /// [`EncodeError::InvalidQuality`] outside `1..=100`.
    pub const fn new(quality: u8, background: [u8; 3]) -> Result<Self, EncodeError> {
        if quality == 0 || quality > 100 {
            return Err(EncodeError::InvalidQuality);
        }
        Ok(Self {
            quality,
            background,
        })
    }

    /// The quality, `1..=100`.
    #[must_use]
    pub const fn quality(&self) -> u8 {
        self.quality
    }

    /// The colour transparency is composited over.
    #[must_use]
    pub const fn background(&self) -> [u8; 3] {
        self.background
    }
}

/// Write `picture` as a PNG in the smallest colour type that holds it
/// exactly: indexed pictures keep their palette and indices, and a
/// truecolour one drops an alpha channel or colour it does not use.
///
/// # Errors
///
/// See [`EncodeError`].
pub fn encode_png(picture: &dyn PictureSource) -> Result<Vec<u8>, EncodeError> {
    png_encode::encode(picture)
}

/// Write `picture` as a baseline JFIF JPEG.
///
/// A picture whose every pixel is grey is written with one component;
/// quality 90 and above keeps full-resolution colour, and below it colour
/// is sampled at half resolution each way, as every camera writes it.
///
/// # Errors
///
/// See [`EncodeError`].
pub fn encode_jpeg(
    picture: &dyn PictureSource,
    options: JpegOptions,
) -> Result<Vec<u8>, EncodeError> {
    jpeg_encode::encode(picture, options)
}

/// Write `sprites` as a RISC OS sprite area file, in order.
///
/// # Errors
///
/// See [`EncodeError`].
pub fn encode_sprite_area(sprites: &[SpriteInput<'_>]) -> Result<Vec<u8>, EncodeError> {
    sprite_encode::encode(sprites)
}

/// A file being written, grown fallibly.
pub(crate) struct Output {
    bytes: Vec<u8>,
}

impl Output {
    pub(crate) const fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    /// Append `data`, growing amortised so a file written in pieces is not
    /// copied quadratically.
    pub(crate) fn push(&mut self, data: &[u8]) -> Result<(), EncodeError> {
        self.bytes
            .try_reserve(data.len())
            .map_err(|_| EncodeError::OutOfMemory)?;
        self.bytes.extend_from_slice(data);
        Ok(())
    }

    pub(crate) fn byte(&mut self, value: u8) -> Result<(), EncodeError> {
        self.push(&[value])
    }

    pub(crate) fn be_u16(&mut self, value: u16) -> Result<(), EncodeError> {
        self.push(&value.to_be_bytes())
    }

    pub(crate) fn be_u32(&mut self, value: u32) -> Result<(), EncodeError> {
        self.push(&value.to_be_bytes())
    }

    pub(crate) fn le_u32(&mut self, value: u32) -> Result<(), EncodeError> {
        self.push(&value.to_le_bytes())
    }

    pub(crate) fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Overwrite the little-endian word at `at`, which was written earlier.
    pub(crate) fn patch_le_u32(&mut self, at: usize, value: u32) {
        if let Some(slot) = self.bytes.get_mut(at..at + 4) {
            slot.copy_from_slice(&value.to_le_bytes());
        }
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// A buffer of `len` zeroes the encoder works in, refused fallibly.
pub(crate) fn scratch(len: usize) -> Result<Vec<u8>, EncodeError> {
    tairix_util::fallible::filled(len, 0u8).ok_or(EncodeError::OutOfMemory)
}

/// Refuse a palette an indexed picture of `depth` cannot have: none, or more
/// colours than `depth` indexes.
pub(crate) const fn palette_fits(depth: IndexDepth, palette: &[Rgba8]) -> Result<(), EncodeError> {
    if palette.is_empty() || palette.len() > depth.colours() {
        Err(EncodeError::InvalidPalette)
    } else {
        Ok(())
    }
}

/// Refuse a row whose `indices` name a colour past the palette's `colours`.
pub(crate) fn indices_fit(indices: &[u8], colours: usize) -> Result<(), EncodeError> {
    if indices.iter().any(|&index| usize::from(index) >= colours) {
        Err(EncodeError::IndexOutOfRange)
    } else {
        Ok(())
    }
}

/// The row buffers a picture is read into: its samples and, for a masked
/// indexed picture, its alpha plane.
pub(crate) struct RowBuffers {
    pub(crate) samples: Vec<u8>,
    pub(crate) mask: Vec<u8>,
}

impl RowBuffers {
    /// Buffers for one row of `source`.
    pub(crate) fn for_source(source: &dyn PictureSource) -> Result<Self, EncodeError> {
        let width = usize::try_from(source.width()).map_err(|_| EncodeError::TooLarge)?;
        let (per_pixel, masked) = match source.kind() {
            crate::PictureKind::Indexed { masked, .. } => (1, masked),
            crate::PictureKind::Rgba => (crate::RGBA_BYTES, false),
        };
        Ok(Self {
            samples: scratch(width.checked_mul(per_pixel).ok_or(EncodeError::TooLarge)?)?,
            mask: if masked { scratch(width)? } else { Vec::new() },
        })
    }

    /// Read row `y` of `source` into the buffers.
    pub(crate) fn read(&mut self, source: &dyn PictureSource, y: u32) {
        source.read_row(y, &mut self.samples, &mut self.mask);
    }
}

#[cfg(test)]
#[path = "encode_tests.rs"]
mod tests;
