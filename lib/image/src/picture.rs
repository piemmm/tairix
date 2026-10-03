//! Pictures in the representation their files store: palette indices with
//! their palette and an optional mask, or straight-alpha RGBA8. That is what
//! an editor holds and what the encoders write, where [`RasterImage`] is only
//! what a picture looks like.
//!
//! [`RasterImage`]: crate::RasterImage

use alloc::vec::Vec;
use core::fmt;

use tairix_util::fallible;

use crate::density::Density;
use crate::RGBA_BYTES;

/// One straight-alpha palette entry: red, green, blue, alpha.
pub type Rgba8 = [u8; 4];

/// `above` composited over `below`, both straight alpha, rounded to the
/// nearest: the one source-over of this pixel format, which an animation's
/// frames and an editor's strokes are both laid down by.
#[must_use]
pub fn over(below: Rgba8, above: Rgba8) -> Rgba8 {
    let source = u32::from(above[3]);
    match source {
        0 => return below,
        255 => return above,
        _ => {}
    }
    let under = (u32::from(below[3]) * (255 - source) + 127) / 255;
    let alpha = source + under;
    let channel = |at: usize| {
        let sum = u32::from(above[at]) * source + u32::from(below[at]) * under;
        u8::try_from((sum + alpha / 2) / alpha).unwrap_or(u8::MAX)
    };
    [
        channel(0),
        channel(1),
        channel(2),
        u8::try_from(alpha).unwrap_or(u8::MAX),
    ]
}

/// Bits one palette index occupies.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum IndexDepth {
    /// Two colours.
    One,
    /// Four colours.
    Two,
    /// Sixteen colours.
    Four,
    /// Two hundred and fifty-six colours.
    Eight,
}

impl IndexDepth {
    /// Every depth, shallowest first.
    pub const ALL: [Self; 4] = [Self::One, Self::Two, Self::Four, Self::Eight];

    /// Bits per index.
    #[must_use]
    pub const fn bits(self) -> u32 {
        match self {
            Self::One => 1,
            Self::Two => 2,
            Self::Four => 4,
            Self::Eight => 8,
        }
    }

    /// Colours an index of this depth can name.
    #[must_use]
    pub const fn colours(self) -> usize {
        1 << self.bits()
    }

    /// The depth of `bits` bits per index.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Option<Self> {
        match bits {
            1 => Some(Self::One),
            2 => Some(Self::Two),
            4 => Some(Self::Four),
            8 => Some(Self::Eight),
            _ => None,
        }
    }

    /// The shallowest depth whose indices name `colours` colours, or `None`
    /// for none or more than 256.
    #[must_use]
    pub fn holding(colours: usize) -> Option<Self> {
        if colours == 0 {
            return None;
        }
        Self::ALL
            .into_iter()
            .find(|depth| depth.colours() >= colours)
    }
}

/// What a picture's pixels are.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PictureKind<'a> {
    /// Palette indices. `palette` names every colour an index may select,
    /// and `masked` says a per-pixel alpha plane rides beside the indices.
    Indexed {
        /// Bits per index.
        depth: IndexDepth,
        /// The colours, at most [`IndexDepth::colours`] of them.
        palette: &'a [Rgba8],
        /// Whether each pixel also carries an alpha value of its own.
        masked: bool,
    },
    /// Straight-alpha RGBA8.
    Rgba,
}

/// A picture read a row at a time, which is how every encoder takes one: a
/// caller holding its pixels in tiles need never assemble them flat.
pub trait PictureSource {
    /// Width, in pixels.
    fn width(&self) -> u32;
    /// Height, in pixels.
    fn height(&self) -> u32;
    /// What the pixels are.
    fn kind(&self) -> PictureKind<'_>;
    /// Copy row `y` out.
    ///
    /// `samples` is `width` indices for an indexed picture or `width * 4`
    /// RGBA bytes; `mask` is `width` alpha values for a masked indexed
    /// picture and empty otherwise. An encoder validates what it is given,
    /// so an index past the palette is refused there rather than trusted.
    fn read_row(&self, y: u32, samples: &mut [u8], mask: &mut [u8]);
    /// How densely the pixels are laid out, where that is stated; an encoder
    /// whose format can say so writes it.
    fn density(&self) -> Option<Density> {
        None
    }
}

/// Why a [`Picture`] could not be built from the parts given.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PictureError {
    /// A zero width or height.
    ZeroDimension,
    /// More pixels than an address space can hold.
    TooLarge,
    /// A buffer's length disagrees with the geometry.
    LengthMismatch,
    /// An indexed picture with no colours.
    EmptyPalette,
    /// More colours than the depth can index.
    PaletteTooLong,
    /// A pixel names a colour past the palette's end.
    IndexOutOfRange,
}

impl fmt::Display for PictureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ZeroDimension => "a picture needs a width and a height",
            Self::TooLarge => "the picture holds more pixels than can be addressed",
            Self::LengthMismatch => "a pixel buffer disagrees with the picture's size",
            Self::EmptyPalette => "an indexed picture needs at least one colour",
            Self::PaletteTooLong => "the palette holds more colours than its depth can name",
            Self::IndexOutOfRange => "a pixel names a colour past the palette's end",
        })
    }
}

/// A picture's pixels, as its file stores them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Pixels {
    /// One index per pixel, every one below `palette.len()`, and an alpha
    /// value per pixel when `mask` is present.
    Indexed {
        /// Bits per index.
        depth: IndexDepth,
        /// The colours the indices select.
        palette: Vec<Rgba8>,
        /// One index per pixel, row-major.
        indices: Vec<u8>,
        /// One alpha value per pixel, row-major.
        mask: Option<Vec<u8>>,
    },
    /// Straight-alpha RGBA8, row-major.
    Rgba(Vec<u8>),
}

/// A whole picture held flat: what a decoder produces.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Picture {
    width: u32,
    height: u32,
    pixels: Pixels,
    density: Option<Density>,
}

/// Pixels in a `width` by `height` picture, refusing a zero side or a count
/// whose RGBA form could not be addressed.
pub(crate) fn pixel_count(width: u32, height: u32) -> Result<usize, PictureError> {
    if width == 0 || height == 0 {
        return Err(PictureError::ZeroDimension);
    }
    usize::try_from(u64::from(width) * u64::from(height))
        .ok()
        .filter(|count| count.checked_mul(RGBA_BYTES).is_some())
        .ok_or(PictureError::TooLarge)
}

impl Picture {
    /// A truecolour picture of `rgba`, four straight-alpha bytes per pixel.
    ///
    /// # Errors
    ///
    /// A zero side, an unaddressable size, or a buffer of the wrong length.
    pub fn rgba(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, PictureError> {
        let count = pixel_count(width, height)?;
        if rgba.len() != count * RGBA_BYTES {
            return Err(PictureError::LengthMismatch);
        }
        Ok(Self {
            width,
            height,
            pixels: Pixels::Rgba(rgba),
            density: None,
        })
    }

    /// An indexed picture: one index per pixel into `palette`, and an alpha
    /// value per pixel when `mask` is given.
    ///
    /// # Errors
    ///
    /// A zero side, an unaddressable size, a buffer of the wrong length, an
    /// empty palette or one longer than `depth` indexes, or an index past
    /// the palette's end.
    pub fn indexed(
        width: u32,
        height: u32,
        depth: IndexDepth,
        palette: Vec<Rgba8>,
        indices: Vec<u8>,
        mask: Option<Vec<u8>>,
    ) -> Result<Self, PictureError> {
        let count = pixel_count(width, height)?;
        if indices.len() != count || mask.as_ref().is_some_and(|mask| mask.len() != count) {
            return Err(PictureError::LengthMismatch);
        }
        if palette.is_empty() {
            return Err(PictureError::EmptyPalette);
        }
        if palette.len() > depth.colours() {
            return Err(PictureError::PaletteTooLong);
        }
        if indices
            .iter()
            .any(|&index| usize::from(index) >= palette.len())
        {
            return Err(PictureError::IndexOutOfRange);
        }
        Ok(Self {
            width,
            height,
            pixels: Pixels::Indexed {
                depth,
                palette,
                indices,
                mask,
            },
            density: None,
        })
    }

    /// Width, in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height, in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The pixels.
    #[must_use]
    pub const fn pixels(&self) -> &Pixels {
        &self.pixels
    }

    /// How densely its pixels are laid out, where that is stated.
    #[must_use]
    pub const fn density(&self) -> Option<Density> {
        self.density
    }

    /// This picture laid out at `density`.
    #[must_use]
    pub const fn with_density(mut self, density: Option<Density>) -> Self {
        self.density = density;
        self
    }

    /// Take the pixels.
    #[must_use]
    pub fn into_pixels(self) -> Pixels {
        self.pixels
    }

    /// The whole picture as straight-alpha RGBA8, or `None` where the
    /// allocator refuses the buffer.
    #[must_use]
    pub fn to_rgba(&self) -> Option<Vec<u8>> {
        let width = usize::try_from(self.width).ok()?;
        let row_bytes = width.checked_mul(RGBA_BYTES)?;
        let mut out = fallible::filled(row_bytes.checked_mul(self.height as usize)?, 0u8)?;
        match &self.pixels {
            Pixels::Rgba(rgba) => out.copy_from_slice(rgba),
            Pixels::Indexed {
                depth,
                palette,
                indices,
                mask,
            } => {
                let kind = PictureKind::Indexed {
                    depth: *depth,
                    palette,
                    masked: mask.is_some(),
                };
                for (y, row) in out.chunks_exact_mut(row_bytes).enumerate() {
                    let at = y * width;
                    let samples = &indices[at..at + width];
                    let alpha = mask.as_ref().map_or(&[][..], |mask| &mask[at..at + width]);
                    flatten_row(kind, samples, alpha, row);
                }
            }
        }
        Some(out)
    }
}

impl PictureSource for Picture {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn kind(&self) -> PictureKind<'_> {
        match &self.pixels {
            Pixels::Indexed {
                depth,
                palette,
                mask,
                ..
            } => PictureKind::Indexed {
                depth: *depth,
                palette,
                masked: mask.is_some(),
            },
            Pixels::Rgba(_) => PictureKind::Rgba,
        }
    }

    fn read_row(&self, y: u32, samples: &mut [u8], mask: &mut [u8]) {
        let width = self.width as usize;
        let row = y as usize;
        match &self.pixels {
            Pixels::Indexed {
                indices,
                mask: plane,
                ..
            } => {
                copy_row(indices, row, width, samples);
                if let Some(plane) = plane {
                    copy_row(plane, row, width, mask);
                }
            }
            Pixels::Rgba(rgba) => copy_row(rgba, row, width * RGBA_BYTES, samples),
        }
    }

    fn density(&self) -> Option<Density> {
        self.density
    }
}

/// Copy row `row` of `plane`, whose rows are `stride` bytes, into as much of
/// `out` as both hold.
fn copy_row(plane: &[u8], row: usize, stride: usize, out: &mut [u8]) {
    let start = row.checked_mul(stride);
    let span = start.and_then(|start| Some(start..start.checked_add(stride)?));
    let Some(src) = span.and_then(|span| plane.get(span)) else {
        return;
    };
    let len = src.len().min(out.len());
    out[..len].copy_from_slice(&src[..len]);
}

/// `entry` seen through a per-pixel alpha of `mask`: the two alphas
/// multiply, rounded to nearest.
#[must_use]
pub fn masked_colour(entry: Rgba8, mask: u8) -> Rgba8 {
    let alpha = (u32::from(entry[3]) * u32::from(mask) + 127) / 255;
    [
        entry[0],
        entry[1],
        entry[2],
        u8::try_from(alpha).unwrap_or(u8::MAX),
    ]
}

/// Expand one row of `kind` pixels to straight-alpha RGBA8 in `out`.
///
/// `samples` and `mask` are a row as [`PictureSource::read_row`] fills
/// them. An index past the palette reads as fully transparent, so a caller
/// flattening a picture it has not validated cannot fault here.
pub fn flatten_row(kind: PictureKind<'_>, samples: &[u8], mask: &[u8], out: &mut [u8]) {
    match kind {
        PictureKind::Rgba => {
            let len = out.len().min(samples.len());
            out[..len].copy_from_slice(&samples[..len]);
        }
        PictureKind::Indexed {
            palette, masked, ..
        } => {
            for (x, (pixel, &index)) in out
                .as_chunks_mut::<RGBA_BYTES>()
                .0
                .iter_mut()
                .zip(samples)
                .enumerate()
            {
                let entry = palette.get(usize::from(index)).copied().unwrap_or([0; 4]);
                *pixel = if masked {
                    masked_colour(entry, mask.get(x).copied().unwrap_or(0))
                } else {
                    entry
                };
            }
        }
    }
}

#[cfg(test)]
#[path = "picture_tests.rs"]
mod tests;
