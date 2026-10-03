//! Writing pictures: PNG, baseline JPEG, GIF, BMP, TIFF, and RISC OS sprite
//! areas.
//!
//! Every encoder reads its picture a row at a time through
//! [`PictureSource`], so a caller holding pixels in tiles never flattens
//! them, and every one validates what it reads — an index past the palette,
//! a mask its format cannot state — refusing rather than writing a file that
//! says something other than the picture.

use alloc::vec::Vec;
use core::fmt;

use tairix_compress::deflate::Flush;
use tairix_compress::zlib::Encoder;
use tairix_util::fallible;

use crate::picture::{flatten_row, IndexDepth, PictureKind, PictureSource, Rgba8};
use crate::sprite_encode::SpriteInput;
use crate::{
    bmp_encode, gif_encode, jpeg_encode, png_encode, sprite_encode, tiff_encode, RGBA_BYTES,
};

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
    /// A format that holds palette colours alone was given colour.
    NotIndexed,
    /// A GIF's palette has every entry on show, leaving none for the
    /// transparent colour its clear pixels need.
    GifPaletteFull,
    /// A TIFF with no pages.
    NoPages,
    /// An OpenRaster document with no layers, or more than are written.
    LayerCount,
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
            Self::NotIndexed => "this format holds palette colours alone",
            Self::GifPaletteFull => {
                "every colour of the palette is on show, leaving none for a GIF's transparency"
            }
            Self::NoPages => "a TIFF needs at least one page",
            Self::LayerCount => "an OpenRaster document holds between one and 256 layers",
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

/// How a GIF is written.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct GifOptions {
    /// Rows in the four interlaced passes, so a reader can show the picture
    /// coarse before it has all of it.
    pub interlaced: bool,
}

/// How a TIFF's strips are compressed.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum TiffCompression {
    /// Not at all.
    None,
    /// `PackBits` run lengths.
    PackBits,
    /// LZW.
    #[default]
    Lzw,
    /// DEFLATE, in zlib streams.
    Deflate,
}

impl TiffCompression {
    /// Every compression, in the order a choice lists them.
    pub const ALL: [Self; 4] = [Self::None, Self::Lzw, Self::Deflate, Self::PackBits];
}

/// How a TIFF is written.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct TiffOptions {
    /// How every strip is compressed.
    pub compression: TiffCompression,
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

/// Write `picture` as a GIF: one frame of its palette indices.
///
/// # Errors
///
/// [`EncodeError::NotIndexed`] for a colour picture, and see [`EncodeError`].
pub fn encode_gif(
    picture: &dyn PictureSource,
    options: GifOptions,
) -> Result<Vec<u8>, EncodeError> {
    gif_encode::encode(picture, options)
}

/// Write `picture` as a BMP: a palette picture at 1, 4 or 8 bits, opaque
/// colour at 24, and anything with transparency at 32 with an alpha mask.
///
/// # Errors
///
/// See [`EncodeError`].
pub fn encode_bmp(picture: &dyn PictureSource) -> Result<Vec<u8>, EncodeError> {
    bmp_encode::encode(picture)
}

/// Write `pages` as a TIFF, one page each, in order.
///
/// # Errors
///
/// [`EncodeError::NoPages`] for none, and see [`EncodeError`].
pub fn encode_tiff(
    pages: &[&dyn PictureSource],
    options: TiffOptions,
) -> Result<Vec<u8>, EncodeError> {
    tiff_encode::encode(pages, options)
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

    pub(crate) fn le_u16(&mut self, value: u16) -> Result<(), EncodeError> {
        self.push(&value.to_le_bytes())
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

/// A zlib encoder on the heap, where its large state belongs.
pub(crate) fn zlib_encoder() -> Result<Vec<Encoder>, EncodeError> {
    let mut encoders = Vec::new();
    if !fallible::reserve(&mut encoders, 1) {
        return Err(EncodeError::OutOfMemory);
    }
    encoders.push(Encoder::new());
    Ok(encoders)
}

/// Compressed bytes a zlib stream has produced and its writer has not yet
/// framed. The buffer is kept at its high-water length, so compressing into
/// it zero-fills each byte once rather than once a call.
pub(crate) struct Deflated {
    pub(crate) bytes: Vec<u8>,
    pub(crate) filled: usize,
}

impl Deflated {
    pub(crate) const fn new() -> Self {
        Self {
            bytes: Vec::new(),
            filled: 0,
        }
    }

    /// Compress `input` under `flush` straight onto the end of what waits.
    pub(crate) fn deflate(
        &mut self,
        encoder: &mut Encoder,
        input: &[u8],
        flush: Flush,
    ) -> Result<(), EncodeError> {
        let end = self
            .filled
            .checked_add(encoder.bound(input.len()))
            .ok_or(EncodeError::OutOfMemory)?;
        if !fallible::grow_to(&mut self.bytes, end, 0) {
            return Err(EncodeError::OutOfMemory);
        }
        let room = self
            .bytes
            .get_mut(self.filled..end)
            .ok_or(EncodeError::OutOfMemory)?;
        self.filled += encoder
            .compress(input, room, flush)
            .map_err(|_| EncodeError::OutOfMemory)?;
        Ok(())
    }

    /// What waits.
    pub(crate) fn waiting(&self) -> &[u8] {
        &self.bytes[..self.filled]
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

/// Validate every index of an indexed picture, answering whether any pixel
/// shows less than opaque through its entry or its mask.
pub(crate) fn indexed_translucent(
    source: &dyn PictureSource,
    rows: &mut RowBuffers,
    palette: &[Rgba8],
    masked: bool,
) -> Result<bool, EncodeError> {
    let mut translucent = false;
    for y in 0..source.height() {
        rows.read(source, y);
        indices_fit(&rows.samples, palette.len())?;
        translucent = translucent
            || rows.samples.iter().enumerate().any(|(x, &index)| {
                palette[usize::from(index)][3] != u8::MAX
                    || (masked && rows.mask.get(x).is_some_and(|&alpha| alpha != u8::MAX))
            });
    }
    Ok(translucent)
}

/// What every pixel of a picture amounts to once flattened to colour.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Survey {
    pub(crate) opaque: bool,
    pub(crate) grey: bool,
    /// For a grey picture, whether every level is exact at one, two and four
    /// bits.
    pub(crate) exact: [bool; 3],
}

/// Read `source` once and survey its colours; an indexed picture's rows are
/// flattened through `rgba`, a row of RGBA bytes.
pub(crate) fn survey(source: &dyn PictureSource, rows: &mut RowBuffers, rgba: &mut [u8]) -> Survey {
    let kind = source.kind();
    let mut survey = Survey {
        opaque: true,
        grey: true,
        exact: [true; 3],
    };
    for y in 0..source.height() {
        rows.read(source, y);
        let pixels = match kind {
            PictureKind::Rgba => &rows.samples,
            PictureKind::Indexed { .. } => {
                flatten_row(kind, &rows.samples, &rows.mask, rgba);
                &*rgba
            }
        };
        for pixel in pixels.as_chunks::<RGBA_BYTES>().0 {
            survey.opaque &= pixel[3] == u8::MAX;
            if survey.grey && (pixel[0] != pixel[1] || pixel[1] != pixel[2]) {
                survey.grey = false;
            }
            if survey.grey {
                let level = pixel[0];
                survey.exact[0] &= level == 0 || level == u8::MAX;
                survey.exact[1] &= level % 85 == 0;
                survey.exact[2] &= level % 17 == 0;
            }
        }
        if !survey.opaque && !survey.grey {
            break;
        }
    }
    survey
}

/// Pack `values`, `bits` wide each, most significant first, into `out`.
pub(crate) fn pack(values: impl Iterator<Item = u8>, bits: u32, out: &mut [u8]) {
    out.fill(0);
    // Counted in `u64`: a row may be wider than a `u32` holds bits for.
    let mut bit = 0u64;
    for value in values {
        let Ok(at) = usize::try_from(bit / 8) else {
            return;
        };
        if let Some(byte) = out.get_mut(at) {
            *byte |= value << (8 - u64::from(bits) - bit % 8);
        }
        bit += u64::from(bits);
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
            PictureKind::Indexed { masked, .. } => (1, masked),
            PictureKind::Rgba => (RGBA_BYTES, false),
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
