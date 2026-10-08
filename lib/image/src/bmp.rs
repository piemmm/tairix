//! A complete, fail-closed BMP decoder, shared with the ICO and CUR
//! containers that carry device-independent bitmaps of their own.
//!
//! A BMP file is a `BITMAPFILEHEADER` naming where the pixel array starts,
//! then a DIB header, then an optional colour table, then the rows. An icon
//! carries the same thing from the DIB header onward and finds its pixels
//! straight after the colour table, so everything below the file header is
//! the shared part: [`read_dib`] and [`decode_pixels`] are what the icon
//! container reaches for.
//!
//! The header lengths claimed are the Windows lineage — `BITMAPCOREHEADER`,
//! `BITMAPINFOHEADER`, `BITMAPV2INFOHEADER`, `BITMAPV3INFOHEADER`,
//! `BITMAPV4HEADER`, `BITMAPV5HEADER`. The OS/2 2.x lineage shares their
//! prefix but reads compression codes 3 and 4 as Huffman 1D and RLE24, two
//! codecs with no other consumer here, so its header lengths are refused by
//! name rather than half-read as something they are not.
//!
//! # Two places this decoder does not read the specification literally
//!
//! A 32-bit `BI_RGB` pixel's fourth byte is undefined, so a BMP file's is
//! ignored and its pixels come out opaque. An icon's is its alpha channel,
//! which the container asks for with [`Dib::with_top_byte_alpha`]: the file
//! header is what tells the two apart, and only the container has it.
//!
//! Pixels a run-length-encoded array never covers — past a delta, after a
//! short line, or beyond an end-of-bitmap — stay fully transparent. The
//! format gives them no value, and the alternative is inventing one.

use alloc::vec::Vec;

use tairix_raster::{RowOrder, RowReducer};
use tairix_util::fallible;

use crate::channel::{Channel, Sampler};
use crate::density::{Density, DensityUnit};
use crate::picture::{IndexDepth, Picture};
use crate::{DecodeError, DecodeLimits, FitBox, RasterImage, Unkept, PROBE_LIMITS, RGBA_BYTES};

/// The two magic bytes every BMP file opens with.
pub(crate) const MAGIC: [u8; 2] = *b"BM";

/// `BITMAPFILEHEADER`'s fixed length, and the offset within it of the
/// `bfOffBits` field naming where the pixel array starts.
pub(crate) const FILE_HEADER_LEN: usize = 14;
const FILE_HEADER_PIXEL_OFFSET_AT: usize = 10;

/// The DIB header lengths this decoder claims, by name.
const CORE_HEADER_LEN: u32 = 12;
pub(crate) const INFO_HEADER_LEN: u32 = 40;
const V2_HEADER_LEN: u32 = 52;
const V3_HEADER_LEN: u32 = 56;
pub(crate) const V4_HEADER_LEN: u32 = 108;
const V5_HEADER_LEN: u32 = 124;

/// Where the channel masks sit, whether as fields of the header (from
/// `BITMAPV2INFOHEADER` on) or as the block following a `BITMAPINFOHEADER`
/// that declares `BI_BITFIELDS`. Both spellings put them in the same place.
const MASK_AT: [usize; RGBA_BYTES] = [40, 44, 48, 52];

/// Pixel-array encodings, as `biCompression` spells them.
pub(crate) const BI_RGB: u32 = 0;
const BI_RLE8: u32 = 1;
const BI_RLE4: u32 = 2;
pub(crate) const BI_BITFIELDS: u32 = 3;
const BI_ALPHABITFIELDS: u32 = 6;

/// Where a `BITMAPINFOHEADER`'s density figures sit: pixels per metre across
/// and down.
const DENSITY_AT: [usize; 2] = [24, 28];

/// Where a `BITMAPV4HEADER` states its colour space, and the span of the
/// endpoints and gamma a calibrated one carries after it.
const COLOUR_SPACE_AT: usize = 56;
pub(crate) const CALIBRATION: core::ops::Range<usize> = 60..108;

/// `bV4CSType`: calibrated by the endpoints that follow, sRGB, and the
/// system's own space.
const LCS_CALIBRATED_RGB: u32 = 0;
pub(crate) const LCS_SRGB: u32 = 0x7352_4742;
const LCS_WINDOWS_COLOR_SPACE: u32 = 0x5769_6E20;

/// The escapes a run-length-encoded array uses in place of a run length.
const RLE_END_OF_LINE: u8 = 0;
const RLE_END_OF_BITMAP: u8 = 1;
const RLE_DELTA: u8 = 2;

/// Bytes per colour-table entry: three for a `BITMAPCOREHEADER`'s
/// `RGBTRIPLE`, four for every later header's `RGBQUAD`.
const CORE_PALETTE_ENTRY_LEN: u32 = 3;
const PALETTE_ENTRY_LEN: u32 = 4;

fn read_u16(data: &[u8], at: usize) -> Result<u16, DecodeError> {
    crate::le_u16(data, at).ok_or(DecodeError::BmpTruncated)
}

fn read_u32(data: &[u8], at: usize) -> Result<u32, DecodeError> {
    crate::le_u32(data, at).ok_or(DecodeError::BmpTruncated)
}

fn read_i32(data: &[u8], at: usize) -> Result<i32, DecodeError> {
    read_u32(data, at).map(u32::cast_signed)
}

/// How a DIB's pixel array is encoded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Compression {
    /// Uncompressed rows whose channel layout the bit count fixes.
    Rgb,
    /// Uncompressed rows whose channel layout explicit masks give.
    Bitfields,
    /// Eight-bit run-length encoding.
    Rle8,
    /// Four-bit run-length encoding.
    Rle4,
}

/// How one pixel's bits sit in a row.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Packing {
    /// A colour-table index of 1, 2, 4, or 8 bits, most significant first.
    Indexed { bits: u32 },
    /// A little-endian value of 2, 3, or 4 bytes, its channels cut out by
    /// the masks.
    Packed { bytes: u32 },
}

/// The most bytes a colour table holds once resolved: an eight-bit index's
/// reach of RGBA entries.
pub(crate) const RESOLVED_PALETTE_BYTES: u64 = (1 << 8) * RGBA_BYTES as u64;

/// A DIB's colour table: the entries present, and how wide each is.
struct Palette<'a> {
    entries: &'a [u8],
    count: u32,
    entry_len: u32,
}

impl Palette<'_> {
    /// The straight-alpha RGBA a colour-table index names. Entries are
    /// stored blue first.
    fn rgba(&self, index: u32) -> Result<[u8; RGBA_BYTES], DecodeError> {
        if index >= self.count {
            return Err(DecodeError::BmpPaletteIndexOutOfRange);
        }
        let entry_len = usize::try_from(self.entry_len).unwrap_or(usize::MAX);
        let at = usize::try_from(index)
            .ok()
            .and_then(|index| index.checked_mul(entry_len))
            .ok_or(DecodeError::DimensionsOverflow)?;
        let end = at
            .checked_add(entry_len)
            .ok_or(DecodeError::DimensionsOverflow)?;
        let (Some(&blue), Some(&green), Some(&red)) = self
            .entries
            .get(at..end)
            .map_or((None, None, None), |entry| {
                (entry.first(), entry.get(1), entry.get(2))
            })
        else {
            return Err(DecodeError::BmpPaletteIndexOutOfRange);
        };
        Ok([red, green, blue, u8::MAX])
    }

    /// Every entry as straight-alpha RGBA, resolved once.
    fn colours(&self) -> Result<Vec<[u8; RGBA_BYTES]>, DecodeError> {
        let count = usize::try_from(self.count).map_err(|_| DecodeError::DimensionsOverflow)?;
        let mut colours =
            fallible::filled(count, [0u8; RGBA_BYTES]).ok_or(DecodeError::OutOfMemory)?;
        for (index, colour) in (0..).zip(colours.iter_mut()) {
            *colour = self.rgba(index)?;
        }
        Ok(colours)
    }
}

/// A device-independent bitmap's header: the geometry, encoding, and channel
/// layout a pixel reader needs, every field already validated.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Dib {
    width: u32,
    /// Rows the pixel array holds. An icon's DIB declares twice its
    /// picture's height, the second half being its mask.
    height: u32,
    top_down: bool,
    packing: Packing,
    compression: Compression,
    channels: [Channel; RGBA_BYTES],
    palette_entries: u32,
    palette_entry_len: u32,
    /// Bytes from the header's own start to the colour table's.
    palette_at: usize,
}

impl Dib {
    pub(crate) const fn width(&self) -> u32 {
        self.width
    }

    pub(crate) const fn height(&self) -> u32 {
        self.height
    }

    /// Whether the first row of the pixel array is the picture's top one.
    pub(crate) const fn top_down(&self) -> bool {
        self.top_down
    }

    /// Bits one pixel occupies.
    pub(crate) const fn bits(&self) -> u32 {
        match self.packing {
            Packing::Indexed { bits } => bits,
            Packing::Packed { bytes } => bytes * 8,
        }
    }

    /// Bytes from the header's own start to the colour table's.
    pub(crate) const fn palette_at(&self) -> usize {
        self.palette_at
    }

    /// Bytes the colour table occupies between the header and the pixels.
    pub(crate) fn palette_bytes(&self) -> Result<usize, DecodeError> {
        let bytes = u64::from(self.palette_entries)
            .checked_mul(u64::from(self.palette_entry_len))
            .ok_or(DecodeError::DimensionsOverflow)?;
        usize::try_from(bytes).map_err(|_| DecodeError::DimensionsOverflow)
    }

    /// Whether pixels carry an alpha channel of their own.
    pub(crate) const fn has_alpha(&self) -> bool {
        self.channels[3].present()
    }

    /// Take a 32-bit `BI_RGB` pixel's fourth byte as alpha.
    ///
    /// The specification leaves that byte undefined, so a BMP file's stays
    /// ignored; every icon written since Windows XP carries its alpha there.
    pub(crate) const fn with_top_byte_alpha(mut self) -> Self {
        if matches!(self.compression, Compression::Rgb)
            && matches!(self.packing, Packing::Packed { bytes: 4 })
            && !self.has_alpha()
        {
            self.channels[3] = Channel::fixed(24, 8);
        }
        self
    }
}

/// Bytes one row of `width` pixels at `bits` occupies, rows being padded out
/// to a four-byte boundary.
pub(crate) fn stride(width: u32, bits: u32) -> Result<usize, DecodeError> {
    let bytes = u64::from(width)
        .checked_mul(u64::from(bits))
        .and_then(|total| total.checked_add(31))
        .ok_or(DecodeError::DimensionsOverflow)?
        / 32
        * 4;
    usize::try_from(bytes).map_err(|_| DecodeError::DimensionsOverflow)
}

/// The channel layout a bit count implies with no masks to say otherwise:
/// 5-5-5 at sixteen bits and 8-8-8 blue-first at twenty-four and thirty-two,
/// with nothing addressing a thirty-two-bit pixel's fourth byte.
fn default_channels(bits: u32) -> [Channel; RGBA_BYTES] {
    let (red, green, blue) = match bits {
        16 => (
            Channel::fixed(10, 5),
            Channel::fixed(5, 5),
            Channel::fixed(0, 5),
        ),
        24 | 32 => (
            Channel::fixed(16, 8),
            Channel::fixed(8, 8),
            Channel::fixed(0, 8),
        ),
        _ => (Channel::ABSENT, Channel::ABSENT, Channel::ABSENT),
    };
    [red, green, blue, Channel::ABSENT]
}

/// Resolve a bit count into the packing it names, refusing one the format
/// does not define.
fn packing_for(bits: u32) -> Result<Packing, DecodeError> {
    match bits {
        1 | 2 | 4 | 8 => Ok(Packing::Indexed { bits }),
        16 => Ok(Packing::Packed { bytes: 2 }),
        24 => Ok(Packing::Packed { bytes: 3 }),
        32 => Ok(Packing::Packed { bytes: 4 }),
        _ => Err(DecodeError::BmpUnsupportedBitCount),
    }
}

/// Read a DIB header from the start of `data`.
pub(crate) fn read_dib(data: &[u8]) -> Result<Dib, DecodeError> {
    match read_u32(data, 0)? {
        CORE_HEADER_LEN => read_core(data),
        size
        @ (INFO_HEADER_LEN | V2_HEADER_LEN | V3_HEADER_LEN | V4_HEADER_LEN | V5_HEADER_LEN) => {
            read_info(data, size)
        }
        _ => Err(DecodeError::BmpUnsupportedHeaderSize),
    }
}

/// `BITMAPCOREHEADER`: unsigned 16-bit geometry, so always bottom-up; no
/// compression field; and a colour table that is always the bit count's full
/// complement of three-byte entries.
fn read_core(data: &[u8]) -> Result<Dib, DecodeError> {
    if read_u16(data, 8)? != 1 {
        return Err(DecodeError::BmpInvalidPlanes);
    }
    let bits = u32::from(read_u16(data, 10)?);
    let packing = packing_for(bits)?;
    Ok(Dib {
        width: u32::from(read_u16(data, 4)?),
        height: u32::from(read_u16(data, 6)?),
        top_down: false,
        packing,
        compression: Compression::Rgb,
        channels: default_channels(bits),
        palette_entries: if bits <= 8 { 1 << bits } else { 0 },
        palette_entry_len: CORE_PALETTE_ENTRY_LEN,
        palette_at: usize::try_from(CORE_HEADER_LEN).unwrap_or(usize::MAX),
    })
}

/// `BITMAPINFOHEADER` and the `BITMAPV2`/`V3`/`V4`/`V5` headers extending it.
/// The fields those add past the masks — colour space, endpoints, gamma,
/// rendering intent, embedded profile — describe how to interpret colour
/// rather than where the pixels are, so they are read past.
fn read_info(data: &[u8], size: u32) -> Result<Dib, DecodeError> {
    let width = read_i32(data, 4)?;
    let height = read_i32(data, 8)?;
    if read_u16(data, 12)? != 1 {
        return Err(DecodeError::BmpInvalidPlanes);
    }
    let bits = u32::from(read_u16(data, 14)?);
    let packing = packing_for(bits)?;
    let code = read_u32(data, 16)?;
    let top_down = height.is_negative();

    let compression = match code {
        BI_RGB => Compression::Rgb,
        BI_BITFIELDS | BI_ALPHABITFIELDS => Compression::Bitfields,
        BI_RLE8 => Compression::Rle8,
        BI_RLE4 => Compression::Rle4,
        _ => return Err(DecodeError::BmpUnsupportedCompression),
    };
    let consistent = match compression {
        Compression::Rgb => true,
        Compression::Bitfields => bits == 16 || bits == 32,
        Compression::Rle8 => bits == 8 && !top_down,
        Compression::Rle4 => bits == 4 && !top_down,
    };
    if !consistent {
        return Err(DecodeError::BmpCompressionMismatch);
    }

    // A `BITMAPV3INFOHEADER` and later carry an alpha mask of their own; a
    // `BITMAPINFOHEADER` or `BITMAPV2INFOHEADER` gets one only where
    // `BI_ALPHABITFIELDS` asks for it, in the slot the later headers use.
    // With `BI_RGB` the mask fields are not the pixel layout, so the bit
    // count's own default stands.
    let bitfields = matches!(compression, Compression::Bitfields);
    let alpha_mask = bitfields && (code == BI_ALPHABITFIELDS || size >= V3_HEADER_LEN);
    let channels = if bitfields {
        read_masks(data, bits, alpha_mask)?
    } else {
        default_channels(bits)
    };
    let masks_end = match (bitfields, alpha_mask) {
        (false, _) => size,
        (true, false) => V2_HEADER_LEN,
        (true, true) => V3_HEADER_LEN,
    };

    let clr_used = read_u32(data, 32)?;
    let palette_entries = if clr_used == 0 {
        if bits <= 8 {
            1 << bits
        } else {
            0
        }
    } else {
        if bits <= 8 && clr_used > 1 << bits {
            return Err(DecodeError::BmpInvalidPaletteLength);
        }
        clr_used
    };

    Ok(Dib {
        width: u32::try_from(width).map_err(|_| DecodeError::BmpInvalidDimensions)?,
        height: height.unsigned_abs(),
        top_down,
        packing,
        compression,
        channels,
        palette_entries,
        palette_entry_len: PALETTE_ENTRY_LEN,
        palette_at: usize::try_from(size.max(masks_end)).unwrap_or(usize::MAX),
    })
}

/// Read the red, green, blue, and optional alpha masks, refusing a set that
/// leaves a colour channel unaddressed, reaches outside the pixel, or names
/// one bit twice.
fn read_masks(
    data: &[u8],
    bits: u32,
    alpha_mask: bool,
) -> Result<[Channel; RGBA_BYTES], DecodeError> {
    let present = if alpha_mask {
        RGBA_BYTES
    } else {
        RGBA_BYTES - 1
    };
    let mut raw = [0u32; RGBA_BYTES];
    for (slot, at) in raw.iter_mut().zip(MASK_AT).take(present) {
        *slot = read_u32(data, at)?;
    }
    let addressable = u32::MAX >> (u32::BITS - bits.min(u32::BITS));
    let mut seen = 0u32;
    for (index, &mask) in raw.iter().enumerate() {
        let colour = index < RGBA_BYTES - 1;
        if (mask == 0 && colour) || mask & !addressable != 0 || mask & seen != 0 {
            return Err(DecodeError::BmpInvalidMask);
        }
        seen |= mask;
    }
    Ok([
        Channel::new(raw[0]).ok_or(DecodeError::BmpInvalidMask)?,
        Channel::new(raw[1]).ok_or(DecodeError::BmpInvalidMask)?,
        Channel::new(raw[2]).ok_or(DecodeError::BmpInvalidMask)?,
        Channel::new(raw[3]).ok_or(DecodeError::BmpInvalidMask)?,
    ])
}

/// Expand `rows` rows of a DIB's pixel array into a straight-alpha RGBA8
/// buffer, top row first.
///
/// Answers the buffer and how many bytes of `data` the pixel array occupied,
/// which is where an icon's mask follows it. The geometry is weighed against
/// `limits` before the buffer is allocated, so a header that lies about its
/// size cannot make this reserve memory proportional to the lie.
pub(crate) fn decode_pixels(
    dib: &Dib,
    palette: &[u8],
    data: &[u8],
    rows: u32,
    limits: &DecodeLimits,
) -> Result<(Vec<u8>, usize), DecodeError> {
    limits.check(dib.width, rows)?;
    let row_bytes = row_bytes(dib.width)?;
    let out_len = usize::try_from(rows)
        .ok()
        .and_then(|rows| rows.checked_mul(row_bytes))
        .ok_or(DecodeError::DimensionsOverflow)?;
    let mut out = fallible::filled(out_len, 0u8).ok_or(DecodeError::OutOfMemory)?;
    let consumed = stream_rows(dib, palette, data, rows, |y, line| {
        let at = usize::try_from(y)
            .ok()
            .and_then(|y| y.checked_mul(row_bytes))
            .ok_or(DecodeError::DimensionsOverflow)?;
        out.get_mut(at..at + row_bytes)
            .ok_or(DecodeError::DimensionsOverflow)?
            .copy_from_slice(line);
        Ok(())
    })?;
    Ok((out, consumed))
}

/// The straight-alpha RGBA8 bytes of one `width`-pixel row.
fn row_bytes(width: u32) -> Result<usize, DecodeError> {
    usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(RGBA_BYTES))
        .ok_or(DecodeError::DimensionsOverflow)
}

/// Hand each of the `rows` rows of a DIB's pixel array to `row` as
/// straight-alpha RGBA8, with its row counted from the top, in the order the
/// array stores them; answers the bytes the array occupied. One row is held
/// at a time. A run-length array's row is handed over once its runs move past
/// it, a pixel no run covers left clear.
fn stream_rows(
    dib: &Dib,
    palette: &[u8],
    data: &[u8],
    rows: u32,
    mut row: impl FnMut(u32, &[u8]) -> Result<(), DecodeError>,
) -> Result<usize, DecodeError> {
    let mut line = fallible::filled(row_bytes(dib.width)?, 0u8).ok_or(DecodeError::OutOfMemory)?;
    if let Packing::Packed { bytes } = dib.packing {
        let (lines, needed) = uncompressed_rows(dib, data, rows)?;
        let samplers = dib.channels.map(Sampler::new);
        for (y, src) in lines {
            expand_packed(bytes, dib.width, &samplers, src, &mut line)?;
            row(y, &line)?;
        }
        return Ok(needed);
    }
    let colours = Palette {
        entries: palette,
        count: dib.palette_entries,
        entry_len: dib.palette_entry_len,
    }
    .colours()?;
    // Storage order: a run-length array is always bottom-up.
    let stored = |handed: u32| {
        if dib.top_down {
            handed
        } else {
            rows - 1 - handed
        }
    };
    let mut handed = 0u32;
    let consumed = walk_indices(dib, data, rows, |x, y, index| {
        // The walk never returns to a row it has left, so rows before this
        // pixel's are complete.
        while y != stored(handed) {
            if handed + 1 >= rows {
                return Err(DecodeError::BmpRleOutOfBounds);
            }
            row(stored(handed), &line)?;
            line.fill(0);
            handed += 1;
        }
        let colour = usize::try_from(index)
            .ok()
            .and_then(|index| colours.get(index))
            .ok_or(DecodeError::BmpPaletteIndexOutOfRange)?;
        let at = usize::try_from(x)
            .ok()
            .and_then(|x| x.checked_mul(RGBA_BYTES))
            .ok_or(DecodeError::DimensionsOverflow)?;
        line.get_mut(at..at + RGBA_BYTES)
            .ok_or(DecodeError::BmpRleOutOfBounds)?
            .copy_from_slice(colour);
        Ok(())
    })?;
    while handed < rows {
        row(stored(handed), &line)?;
        line.fill(0);
        handed += 1;
    }
    Ok(consumed)
}

/// The uncompressed rows of a pixel array of `rows` rows, top first, and the
/// bytes they occupy.
fn uncompressed_rows<'d>(
    dib: &Dib,
    data: &'d [u8],
    rows: u32,
) -> Result<(impl Iterator<Item = (u32, &'d [u8])> + use<'d>, usize), DecodeError> {
    let stride = stride(dib.width, dib.bits())?;
    let needed = usize::try_from(rows)
        .ok()
        .and_then(|rows| rows.checked_mul(stride))
        .ok_or(DecodeError::DimensionsOverflow)?;
    let data = data
        .get(..needed)
        .ok_or(DecodeError::BmpPixelDataTruncated)?;
    let top_down = dib.top_down;
    let lines = (0..rows)
        .zip(data.chunks_exact(stride.max(1)))
        .map(move |(stored, line)| (if top_down { stored } else { rows - 1 - stored }, line));
    Ok((lines, needed))
}

/// Hand every pixel of an indexed pixel array of `rows` rows to `put` as its
/// column, its row from the top, and its colour-table index; answers the
/// bytes the array occupied. A run-length array's pixels are handed over in
/// whatever order the runs cover them, and one it never covers not at all.
fn walk_indices(
    dib: &Dib,
    data: &[u8],
    rows: u32,
    mut put: impl FnMut(u32, u32, u32) -> Result<(), DecodeError>,
) -> Result<usize, DecodeError> {
    let bits = dib.bits();
    match dib.compression {
        Compression::Rle8 | Compression::Rle4 => read_rle(
            matches!(dib.compression, Compression::Rle4),
            data,
            dib.width,
            rows,
            put,
        ),
        Compression::Rgb | Compression::Bitfields => {
            let (lines, needed) = uncompressed_rows(dib, data, rows)?;
            let per_byte = 8 / bits;
            let mask = u32::MAX >> (u32::BITS - bits);
            for (row, src) in lines {
                for x in 0..dib.width {
                    let at = usize::try_from(x / per_byte).unwrap_or(usize::MAX);
                    let byte = src
                        .get(at)
                        .copied()
                        .ok_or(DecodeError::BmpPixelDataTruncated)?;
                    put(
                        x,
                        row,
                        u32::from(byte) >> (8 - bits * (x % per_byte + 1)) & mask,
                    )?;
                }
            }
            Ok(needed)
        }
    }
}

/// Expand one row of packed pixels through the channel masks.
fn expand_packed(
    bytes: u32,
    width: u32,
    samplers: &[Sampler; RGBA_BYTES],
    src: &[u8],
    dst: &mut [u8],
) -> Result<(), DecodeError> {
    let bytes = usize::try_from(bytes).unwrap_or(usize::MAX);
    let row = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(bytes))
        .ok_or(DecodeError::DimensionsOverflow)?;
    let src = src.get(..row).ok_or(DecodeError::BmpPixelDataTruncated)?;
    for (field, pixel) in src
        .chunks_exact(bytes)
        .zip(dst.as_chunks_mut::<RGBA_BYTES>().0)
    {
        let mut raw = 0u32;
        for &byte in field.iter().rev() {
            raw = raw << 8 | u32::from(byte);
        }
        *pixel = [
            samplers[0].sample(raw),
            samplers[1].sample(raw),
            samplers[2].sample(raw),
            samplers[3].sample(raw),
        ];
    }
    Ok(())
}

/// Walk a run-length-encoded pixel array of `width` by `rows`, handing each
/// pixel it covers to `put` as its column, its row from the top, and its
/// index; answers the bytes it occupied.
///
/// Each step consumes at least two bytes, so the walk always terminates.
/// Every pixel is placed against the picture's own bounds, so a run, delta,
/// or line reaching outside it is refused rather than clipped. The array is
/// bottom-up, so its row `y` is the picture's row `rows - 1 - y`.
fn read_rle(
    four_bit: bool,
    data: &[u8],
    width: u32,
    rows: u32,
    mut put: impl FnMut(u32, u32, u32) -> Result<(), DecodeError>,
) -> Result<usize, DecodeError> {
    let mut place = |x: u32, y: u32, index: u32| {
        if x >= width || y >= rows {
            return Err(DecodeError::BmpRleOutOfBounds);
        }
        put(x, rows - 1 - y, index)
    };
    let mut x = 0u32;
    let mut y = 0u32;
    let mut pos = 0usize;
    loop {
        let Some(&[count, value]) = data.get(pos..).and_then(<[u8]>::first_chunk::<2>) else {
            // A stream stopping once every row is covered simply omitted its
            // end-of-bitmap escape; one stopping earlier is truncated.
            return if y >= rows {
                Ok(pos)
            } else {
                Err(DecodeError::BmpRleTruncated)
            };
        };
        pos += 2;
        if count > 0 {
            for step in 0..u32::from(count) {
                let index = if four_bit {
                    u32::from(if step % 2 == 0 {
                        value >> 4
                    } else {
                        value & 0x0F
                    })
                } else {
                    u32::from(value)
                };
                let column = x.checked_add(step).ok_or(DecodeError::BmpRleOutOfBounds)?;
                place(column, y, index)?;
            }
            x = x
                .checked_add(u32::from(count))
                .ok_or(DecodeError::BmpRleOutOfBounds)?;
            continue;
        }
        match value {
            RLE_END_OF_LINE => {
                x = 0;
                y = y.checked_add(1).ok_or(DecodeError::BmpRleOutOfBounds)?;
            }
            RLE_END_OF_BITMAP => return Ok(pos),
            RLE_DELTA => {
                let Some(&[dx, dy]) = data.get(pos..).and_then(<[u8]>::first_chunk::<2>) else {
                    return Err(DecodeError::BmpRleTruncated);
                };
                pos += 2;
                x = x
                    .checked_add(u32::from(dx))
                    .ok_or(DecodeError::BmpRleOutOfBounds)?;
                y = y
                    .checked_add(u32::from(dy))
                    .ok_or(DecodeError::BmpRleOutOfBounds)?;
            }
            literal => {
                let run_bytes = if four_bit {
                    usize::from(literal).div_ceil(2)
                } else {
                    usize::from(literal)
                };
                let run = data
                    .get(pos..)
                    .and_then(|rest| rest.get(..run_bytes))
                    .ok_or(DecodeError::BmpRleTruncated)?;
                for step in 0..u32::from(literal) {
                    let at = usize::try_from(if four_bit { step / 2 } else { step })
                        .unwrap_or(usize::MAX);
                    let byte = run.get(at).copied().ok_or(DecodeError::BmpRleTruncated)?;
                    let index = if four_bit {
                        u32::from(if step % 2 == 0 {
                            byte >> 4
                        } else {
                            byte & 0x0F
                        })
                    } else {
                        u32::from(byte)
                    };
                    let column = x.checked_add(step).ok_or(DecodeError::BmpRleOutOfBounds)?;
                    place(column, y, index)?;
                }
                x = x
                    .checked_add(u32::from(literal))
                    .ok_or(DecodeError::BmpRleOutOfBounds)?;
                // An absolute run is padded out to a two-byte boundary.
                pos = pos
                    .checked_add(run_bytes.next_multiple_of(2))
                    .ok_or(DecodeError::BmpRleTruncated)?;
                if pos > data.len() {
                    return Err(DecodeError::BmpRleTruncated);
                }
            }
        }
    }
}

/// Split a BMP file into its header, colour table, and pixel array.
fn parts(bytes: &[u8]) -> Result<(Dib, &[u8], &[u8]), DecodeError> {
    if !bytes.starts_with(&MAGIC) {
        return Err(DecodeError::BmpBadSignature);
    }
    // `bfSize` and the two reserved fields are wrong in enough real files to
    // be worthless, and nothing here is sized from them, so they are read
    // past rather than validated.
    let pixel_offset = usize::try_from(read_u32(bytes, FILE_HEADER_PIXEL_OFFSET_AT)?)
        .map_err(|_| DecodeError::BmpInvalidPixelOffset)?;
    let dib = read_dib(
        bytes
            .get(FILE_HEADER_LEN..)
            .ok_or(DecodeError::BmpTruncated)?,
    )?;
    let palette_start = FILE_HEADER_LEN
        .checked_add(dib.palette_at)
        .ok_or(DecodeError::BmpTruncated)?;
    // Above eight bits a pixel carries its own colour, so `biClrUsed` names
    // at most a palette-optimisation hint the file header already points
    // past; at or below, the table has to fit in the gap the header leaves.
    let palette = if matches!(dib.packing, Packing::Indexed { .. }) {
        let end = palette_start
            .checked_add(dib.palette_bytes()?)
            .ok_or(DecodeError::BmpTruncated)?;
        if end > pixel_offset {
            return Err(DecodeError::BmpInvalidPaletteLength);
        }
        bytes
            .get(palette_start..end)
            .ok_or(DecodeError::BmpTruncated)?
    } else {
        if palette_start > pixel_offset {
            return Err(DecodeError::BmpInvalidPixelOffset);
        }
        &[]
    };
    let pixels = bytes
        .get(pixel_offset..)
        .ok_or(DecodeError::BmpInvalidPixelOffset)?;
    Ok((dib, palette, pixels))
}

/// Read a BMP file as the picture it stores: an indexed one as its indices
/// and colour table, any other as RGBA, with the density its header states.
///
/// A colour space beyond sRGB, a colour-table entry's reserved byte, and a
/// 32-bit pixel's undefined fourth byte, where any is set, are held beside
/// the picture; a channel wider than eight bits is narrowed.
pub(crate) fn decode_native(
    bytes: &[u8],
    limits: &DecodeLimits,
) -> Result<(Picture, Unkept), DecodeError> {
    let (dib, palette, pixels) = parts(bytes)?;
    let header = bytes
        .get(FILE_HEADER_LEN..)
        .ok_or(DecodeError::BmpTruncated)?;
    let size = read_u32(header, 0)?;
    let mut extras = colour_space_held(header, size)?;
    let precision = dib.channels.iter().any(|channel| channel.width() > 8);
    let geometry = |_| DecodeError::DimensionsOverflow;
    let picture = if let Packing::Indexed { bits } = dib.packing {
        limits.check(dib.width, dib.height)?;
        let len = usize::try_from(u64::from(dib.width) * u64::from(dib.height))
            .map_err(|_| DecodeError::DimensionsOverflow)?;
        let mut indices = fallible::filled(len, 0u8).ok_or(DecodeError::OutOfMemory)?;
        let runs = matches!(dib.compression, Compression::Rle8 | Compression::Rle4);
        let mut covered = if runs {
            fallible::filled(len, 0u8).ok_or(DecodeError::OutOfMemory)?
        } else {
            Vec::new()
        };
        let width = dib.width as usize;
        walk_indices(&dib, pixels, dib.height, |x, y, index| {
            let index = u8::try_from(index)
                .ok()
                .filter(|&index| u32::from(index) < dib.palette_entries)
                .ok_or(DecodeError::BmpPaletteIndexOutOfRange)?;
            let at = y as usize * width + x as usize;
            *indices.get_mut(at).ok_or(DecodeError::BmpRleOutOfBounds)? = index;
            if let Some(shown) = covered.get_mut(at) {
                *shown = u8::MAX;
            }
            Ok(())
        })?;
        let colours = Palette {
            entries: palette,
            count: dib.palette_entries,
            entry_len: dib.palette_entry_len,
        }
        .colours()?;
        if dib.palette_entry_len == PALETTE_ENTRY_LEN {
            extras |= palette.as_chunks::<4>().0.iter().any(|entry| entry[3] != 0);
        }
        let depth = IndexDepth::from_bits(bits).ok_or(DecodeError::BmpUnsupportedBitCount)?;
        let mask = (runs && covered.contains(&0)).then_some(covered);
        Picture::indexed(dib.width, dib.height, depth, colours, indices, mask).map_err(geometry)?
    } else {
        let (rgba, _) = decode_pixels(&dib, palette, pixels, dib.height, limits)?;
        extras |= uncovered_bits_set(&dib, pixels)?;
        Picture::rgba(dib.width, dib.height, rgba).map_err(geometry)?
    };
    let unkept = Unkept {
        precision,
        extras,
        converted: false,
    };
    Ok((picture.with_density(density(header, size)), unkept))
}

/// Whether any packed pixel sets a bit no channel covers: the undefined
/// fourth byte of a 32-bit `BI_RGB` pixel, the top bit of a 16-bit one, or
/// what a set of masks leaves out.
fn uncovered_bits_set(dib: &Dib, data: &[u8]) -> Result<bool, DecodeError> {
    let Packing::Packed { bytes } = dib.packing else {
        return Ok(false);
    };
    let covered = dib
        .channels
        .iter()
        .fold(0, |union, channel| union | channel.mask());
    let uncovered = !covered & (u32::MAX >> (u32::BITS - bytes * 8));
    if uncovered == 0 {
        return Ok(false);
    }
    let (lines, _) = uncompressed_rows(dib, data, dib.height)?;
    let width = usize::try_from(bytes).unwrap_or(usize::MAX);
    let span = dib.width as usize * width;
    for (_, line) in lines {
        let set = line.get(..span).is_some_and(|row| {
            row.chunks_exact(width).any(|field| {
                let raw = field
                    .iter()
                    .rev()
                    .fold(0u32, |raw, &byte| raw << 8 | u32::from(byte));
                raw & uncovered != 0
            })
        });
        if set {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The density a header of `size` bytes states, where it states one: both
/// figures positive pixels per metre.
fn density(header: &[u8], size: u32) -> Option<Density> {
    if size < INFO_HEADER_LEN {
        return None;
    }
    let [across, down] = DENSITY_AT.map(|at| {
        read_i32(header, at)
            .ok()
            .and_then(|value| u32::try_from(value).ok())
    });
    Density::whole(across?, down?, DensityUnit::Metre)
}

/// Whether a header of `size` bytes states a colour space beyond sRGB: a
/// calibrated one with endpoints or gamma, a profile, or one not defined.
fn colour_space_held(header: &[u8], size: u32) -> Result<bool, DecodeError> {
    if size < V4_HEADER_LEN {
        return Ok(false);
    }
    Ok(match read_u32(header, COLOUR_SPACE_AT)? {
        LCS_SRGB | LCS_WINDOWS_COLOR_SPACE => false,
        LCS_CALIBRATED_RGB => header
            .get(CALIBRATION)
            .ok_or(DecodeError::BmpTruncated)?
            .iter()
            .any(|&byte| byte != 0),
        _ => true,
    })
}

/// Read a BMP file's declared geometry from its header alone.
pub(crate) fn probe(bytes: &[u8]) -> Result<(u32, u32), DecodeError> {
    if !bytes.starts_with(&MAGIC) {
        return Err(DecodeError::BmpBadSignature);
    }
    let dib = read_dib(
        bytes
            .get(FILE_HEADER_LEN..)
            .ok_or(DecodeError::BmpTruncated)?,
    )?;
    PROBE_LIMITS.check(dib.width, dib.height)?;
    Ok((dib.width, dib.height))
}

/// What decoding a `width`×`height` pixel array into a whole picture holds:
/// the picture, the row being expanded, and a palette picture's colours.
pub(crate) fn picture_peak_bytes(width: u32, height: u32) -> u64 {
    u64::from(width)
        .saturating_mul(u64::from(height).saturating_add(1))
        .saturating_mul(RGBA_BYTES as u64)
        .saturating_add(RESOLVED_PALETTE_BYTES)
}

/// An upper bound of the bytes a [`decode_fitted`] of `bytes` to `fit` holds
/// at once: the row being expanded, a palette picture's colours, and the
/// reduction it is fed into.
///
/// # Errors
///
/// What [`decode_fitted`] would refuse from the header.
pub(crate) fn fitted_peak_bytes(
    bytes: &[u8],
    limits: &DecodeLimits,
    fit: FitBox,
) -> Result<u64, DecodeError> {
    let (width, height) = probe(bytes)?;
    limits.check(width, height)?;
    let reduced = fit.reduction(width, height);
    Ok(u64::from(width)
        .saturating_mul(RGBA_BYTES as u64)
        .saturating_add(RESOLVED_PALETTE_BYTES)
        .saturating_add(RowReducer::peak_bytes((width, height), reduced)))
}

/// Decode a BMP file no smaller than it must be to cover `fit`, its rows
/// streamed through a reduction in the order the file stores them, so the
/// picture is never held. It admits what [`decode`] admits.
///
/// # Errors
///
/// See [`DecodeError`] for every fail-closed refusal reason.
pub(crate) fn decode_fitted(
    bytes: &[u8],
    limits: &DecodeLimits,
    fit: FitBox,
) -> Result<RasterImage, DecodeError> {
    let (dib, palette, pixels) = parts(bytes)?;
    limits.check(dib.width, dib.height)?;
    let (width, height) = fit.reduction(dib.width, dib.height);
    let order = if dib.top_down {
        RowOrder::TopDown
    } else {
        RowOrder::BottomUp
    };
    let mut reducer = RowReducer::new((dib.width, dib.height), (width, height), order)
        .map_err(crate::reduction_refused)?;
    stream_rows(&dib, palette, pixels, dib.height, |_, line| {
        reducer.push_row(line).map_err(crate::reduction_refused)
    })?;
    let out = reducer.finish().map_err(crate::reduction_refused)?;
    Ok(RasterImage::from_parts(width, height, out))
}

/// Decode a BMP file at its natural size.
pub(crate) fn decode(bytes: &[u8], limits: &DecodeLimits) -> Result<RasterImage, DecodeError> {
    let (dib, palette, pixels) = parts(bytes)?;
    let (out, _) = decode_pixels(&dib, palette, pixels, dib.height, limits)?;
    Ok(RasterImage::from_parts(dib.width, dib.height, out))
}

#[cfg(test)]
#[path = "bmp_tests.rs"]
mod tests;
