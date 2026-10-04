//! A TIFF encoder: every picture a page, little-endian, in strips.
//!
//! An opaque palette picture keeps its indices and colour map at its own
//! depth — beyond baseline TIFF at one or two bits, which allows a palette
//! only four or eight, though readers built on libtiff take them; a palette
//! has no opacity, so a translucent one is written as colour. Colour is
//! written as grey where every pixel is grey, and carries an unassociated
//! alpha sample only where a pixel needs one. Eight-bit samples under LZW or
//! DEFLATE are differenced along the row first, which is what makes those
//! codecs pay on photographs.

use alloc::vec::Vec;

use tairix_compress::deflate::Flush;
use tairix_compress::zlib::Encoder;
use tairix_util::fallible;

use crate::density::{Density, DensityUnit};
use crate::encode::{
    indexed_translucent, pack, palette_fits, scratch, survey, zlib_encoder, Deflated, EncodeError,
    Output, RowBuffers, TiffCompression, TiffOptions,
};
use crate::lzw::{CodeSink, Coder, Widen};
use crate::picture::{flatten_row, IndexDepth, PictureKind, PictureSource, Rgba8};
use crate::tiff::{
    COMPRESSION_ADOBE_DEFLATE, COMPRESSION_LZW, COMPRESSION_NONE, COMPRESSION_PACK_BITS, ENTRY_LEN,
    EXTRA_UNASSOCIATED_ALPHA, LONG, PHOTOMETRIC_BLACK_ZERO, PHOTOMETRIC_PALETTE, PHOTOMETRIC_RGB,
    PREDICTOR_HORIZONTAL, RATIONAL, RESOLUTION_CENTIMETRE, RESOLUTION_INCH, RESOLUTION_NONE, SHORT,
    SUBFILE_PAGE, TAG_BITS_PER_SAMPLE, TAG_COLOUR_MAP, TAG_COMPRESSION, TAG_EXTRA_SAMPLES,
    TAG_IMAGE_LENGTH, TAG_IMAGE_WIDTH, TAG_NEW_SUBFILE_TYPE, TAG_PAGE_NUMBER, TAG_PHOTOMETRIC,
    TAG_PLANAR_CONFIGURATION, TAG_PREDICTOR, TAG_RESOLUTION_UNIT, TAG_ROWS_PER_STRIP,
    TAG_SAMPLES_PER_PIXEL, TAG_STRIP_BYTE_COUNTS, TAG_STRIP_OFFSETS, TAG_X_RESOLUTION,
    TAG_Y_RESOLUTION,
};
use crate::RGBA_BYTES;

/// Uncompressed bytes one strip holds at most, unless one row is longer.
const STRIP_BYTES: usize = 64 * 1024;

/// The code at which a TIFF LZW table counts as full: one short of the
/// widest code, as every reader of the format's own schedule expects.
const LZW_LIMIT: u16 = 4094;

/// How one page's pixels are written.
enum Plan<'a> {
    Indexed {
        depth: IndexDepth,
        palette: &'a [Rgba8],
    },
    Colour(Colour),
}

/// The colour a page is written as, from fewest samples to most.
#[derive(Copy, Clone)]
enum Colour {
    Grey,
    GreyAlpha,
    Rgb,
    Rgba,
}

impl Plan<'_> {
    const fn samples(&self) -> u16 {
        match self {
            Self::Indexed { .. } | Self::Colour(Colour::Grey) => 1,
            Self::Colour(Colour::GreyAlpha) => 2,
            Self::Colour(Colour::Rgb) => 3,
            Self::Colour(Colour::Rgba) => 4,
        }
    }

    const fn bits(&self) -> u16 {
        match self {
            Self::Indexed { depth, .. } => match depth {
                IndexDepth::One => 1,
                IndexDepth::Two => 2,
                IndexDepth::Four => 4,
                IndexDepth::Eight => 8,
            },
            Self::Colour(_) => 8,
        }
    }

    const fn photometric(&self) -> u16 {
        match self {
            Self::Indexed { .. } => PHOTOMETRIC_PALETTE,
            Self::Colour(Colour::Grey | Colour::GreyAlpha) => PHOTOMETRIC_BLACK_ZERO,
            Self::Colour(Colour::Rgb | Colour::Rgba) => PHOTOMETRIC_RGB,
        }
    }

    const fn alpha(&self) -> bool {
        matches!(self, Self::Colour(Colour::GreyAlpha | Colour::Rgba))
    }
}

pub(crate) fn encode(
    pages: &[&dyn PictureSource],
    options: TiffOptions,
) -> Result<Vec<u8>, EncodeError> {
    if pages.is_empty() {
        return Err(EncodeError::NoPages);
    }
    let total = u16::try_from(pages.len()).map_err(|_| EncodeError::TooLarge)?;
    let mut out = Output::new();
    out.push(b"II")?;
    out.le_u16(42)?;
    let mut link = out.len();
    out.le_u32(0)?;
    let mut work = Work::new(options.compression)?;
    for (number, page) in (0..total).zip(pages) {
        let multipage = (total > 1).then_some((number, total));
        let ifd = write_page(&mut out, *page, multipage, &mut work)?;
        out.patch_le_u32(link, offset(ifd)?);
        link = ifd + 2 + work.entries.len() * ENTRY_LEN;
    }
    Ok(out.into_bytes())
}

/// A file offset as a TIFF states it.
fn offset(at: usize) -> Result<u32, EncodeError> {
    u32::try_from(at).map_err(|_| EncodeError::TooLarge)
}

/// What every page reuses: the codecs and the record of the page last
/// written.
struct Work {
    compression: TiffCompression,
    coder: Option<Coder>,
    deflate: Option<(Vec<Encoder>, Deflated)>,
    offsets: Vec<u32>,
    counts: Vec<u32>,
    entries: Vec<Entry>,
}

impl Work {
    fn new(compression: TiffCompression) -> Result<Self, EncodeError> {
        let coder = match compression {
            TiffCompression::Lzw => {
                Some(Coder::new(8, Widen::OneEarly, LZW_LIMIT).ok_or(EncodeError::OutOfMemory)?)
            }
            _ => None,
        };
        let deflate = match compression {
            TiffCompression::Deflate => Some((zlib_encoder()?, Deflated::new())),
            _ => None,
        };
        Ok(Self {
            compression,
            coder,
            deflate,
            offsets: Vec::new(),
            counts: Vec::new(),
            entries: Vec::new(),
        })
    }

    /// Compress one strip of `raw`, whose rows are `row_len` bytes, onto
    /// the end of `out`.
    fn strip(&mut self, raw: &[u8], row_len: usize, out: &mut Output) -> Result<(), EncodeError> {
        match self.compression {
            TiffCompression::None => out.push(raw),
            TiffCompression::PackBits => {
                for row in raw.chunks(row_len.max(1)) {
                    pack_bits(row, out)?;
                }
                Ok(())
            }
            TiffCompression::Lzw => {
                let coder = self.coder.as_mut().ok_or(EncodeError::OutOfMemory)?;
                let mut sink = MsbCodes::new(out);
                coder.begin(&mut sink)?;
                for &byte in raw {
                    coder.push(byte, &mut sink)?;
                }
                coder.finish(&mut sink)?;
                sink.finish()
            }
            TiffCompression::Deflate => {
                let (encoders, pending) = self.deflate.as_mut().ok_or(EncodeError::OutOfMemory)?;
                let encoder = encoders.first_mut().ok_or(EncodeError::OutOfMemory)?;
                encoder.reset();
                pending.filled = 0;
                pending.deflate(encoder, raw, Flush::Finish)?;
                out.push(pending.waiting())
            }
        }
    }
}

/// Write one page's strips, the values its directory points at, and the
/// directory, answering where the directory begins.
fn write_page(
    out: &mut Output,
    source: &dyn PictureSource,
    multipage: Option<(u16, u16)>,
    work: &mut Work,
) -> Result<usize, EncodeError> {
    let (width, height) = (source.width(), source.height());
    if width == 0 || height == 0 {
        return Err(EncodeError::TooLarge);
    }
    let kind = source.kind();
    let columns = usize::try_from(width).map_err(|_| EncodeError::TooLarge)?;
    let mut rows = RowBuffers::for_source(source)?;
    let mut rgba = scratch(
        columns
            .checked_mul(RGBA_BYTES)
            .ok_or(EncodeError::TooLarge)?,
    )?;
    let plan = plan(source, kind, &mut rows, &mut rgba)?;
    let samples = usize::from(plan.samples());
    let row_bits = u64::from(width) * u64::from(plan.samples()) * u64::from(plan.bits());
    let row_len = usize::try_from(row_bits.div_ceil(8)).map_err(|_| EncodeError::TooLarge)?;
    let per_strip = (STRIP_BYTES / row_len.max(1)).clamp(1, height as usize);
    let rows_per_strip = u32::try_from(per_strip).map_err(|_| EncodeError::TooLarge)?;
    let predict = matches!(plan, Plan::Colour(_))
        && matches!(
            work.compression,
            TiffCompression::Lzw | TiffCompression::Deflate
        );
    let mut raw = scratch(
        row_len
            .checked_mul(per_strip)
            .ok_or(EncodeError::TooLarge)?,
    )?;
    let strips = height.div_ceil(rows_per_strip) as usize;
    work.offsets.clear();
    work.counts.clear();
    if !fallible::reserve(&mut work.offsets, strips) || !fallible::reserve(&mut work.counts, strips)
    {
        return Err(EncodeError::OutOfMemory);
    }
    let mut y = 0;
    while y < height {
        let take = rows_per_strip.min(height - y);
        let used = row_len * take as usize;
        for (line, row) in raw[..used].chunks_exact_mut(row_len).zip(y..) {
            rows.read(source, row);
            scanline(&plan, kind, &rows, &mut rgba, line);
            if predict {
                for at in (samples..line.len()).rev() {
                    line[at] = line[at].wrapping_sub(line[at - samples]);
                }
            }
        }
        let start = out.len();
        work.offsets.push(offset(start)?);
        work.strip(&raw[..used], row_len, out)?;
        work.counts.push(offset(out.len() - start)?);
        y += take;
    }
    let compression = match work.compression {
        TiffCompression::None => COMPRESSION_NONE,
        TiffCompression::PackBits => COMPRESSION_PACK_BITS,
        TiffCompression::Lzw => COMPRESSION_LZW,
        TiffCompression::Deflate => COMPRESSION_ADOBE_DEFLATE,
    };
    let mut ifd = Directory::new(work);
    ifd.long(
        TAG_NEW_SUBFILE_TYPE,
        if multipage.is_some() { SUBFILE_PAGE } else { 0 },
    )?;
    ifd.long(TAG_IMAGE_WIDTH, width)?;
    ifd.long(TAG_IMAGE_LENGTH, height)?;
    let depths = [plan.bits(); 4];
    ifd.shorts(out, TAG_BITS_PER_SAMPLE, &depths[..samples])?;
    ifd.short(TAG_COMPRESSION, compression)?;
    ifd.short(TAG_PHOTOMETRIC, plan.photometric())?;
    ifd.longs_from_work(out, TAG_STRIP_OFFSETS, Which::Offsets)?;
    ifd.short(TAG_SAMPLES_PER_PIXEL, plan.samples())?;
    ifd.long(TAG_ROWS_PER_STRIP, rows_per_strip)?;
    ifd.longs_from_work(out, TAG_STRIP_BYTE_COUNTS, Which::Counts)?;
    let (unit, across, down) = resolution(source.density());
    ifd.rational(out, TAG_X_RESOLUTION, across)?;
    ifd.rational(out, TAG_Y_RESOLUTION, down)?;
    ifd.short(TAG_PLANAR_CONFIGURATION, 1)?;
    ifd.short(TAG_RESOLUTION_UNIT, unit)?;
    if let Some((number, total)) = multipage {
        ifd.shorts(out, TAG_PAGE_NUMBER, &[number, total])?;
    }
    if predict {
        ifd.short(TAG_PREDICTOR, PREDICTOR_HORIZONTAL)?;
    }
    if let Plan::Indexed { depth, palette } = &plan {
        ifd.colour_map(out, depth.bits(), palette)?;
    }
    if plan.alpha() {
        ifd.short(TAG_EXTRA_SAMPLES, EXTRA_UNASSOCIATED_ALPHA)?;
    }
    ifd.finish(out)
}

/// Read a page once to choose how it is written.
fn plan<'a>(
    source: &dyn PictureSource,
    kind: PictureKind<'a>,
    rows: &mut RowBuffers,
    rgba: &mut [u8],
) -> Result<Plan<'a>, EncodeError> {
    if let PictureKind::Indexed {
        depth,
        palette,
        masked,
    } = kind
    {
        palette_fits(depth, palette)?;
        if !indexed_translucent(source, rows, palette, masked)? {
            return Ok(Plan::Indexed { depth, palette });
        }
    }
    let survey = survey(source, rows, rgba);
    Ok(Plan::Colour(match (survey.grey, survey.opaque) {
        (true, true) => Colour::Grey,
        (true, false) => Colour::GreyAlpha,
        (false, true) => Colour::Rgb,
        (false, false) => Colour::Rgba,
    }))
}

/// Write one row of `plan` from the buffers its source row was read into.
fn scanline(
    plan: &Plan<'_>,
    kind: PictureKind<'_>,
    rows: &RowBuffers,
    rgba: &mut [u8],
    line: &mut [u8],
) {
    let colour = match plan {
        Plan::Indexed { depth, .. } => {
            pack(rows.samples.iter().copied(), depth.bits(), line);
            return;
        }
        Plan::Colour(colour) => *colour,
    };
    let pixels = match kind {
        PictureKind::Rgba => &rows.samples,
        PictureKind::Indexed { .. } => {
            flatten_row(kind, &rows.samples, &rows.mask, rgba);
            &*rgba
        }
    };
    let pixels = pixels.as_chunks::<RGBA_BYTES>().0;
    match colour {
        Colour::Grey => {
            for (slot, pixel) in line.iter_mut().zip(pixels) {
                *slot = pixel[0];
            }
        }
        Colour::GreyAlpha => {
            for (slot, pixel) in line.as_chunks_mut::<2>().0.iter_mut().zip(pixels) {
                *slot = [pixel[0], pixel[3]];
            }
        }
        Colour::Rgb => {
            for (slot, pixel) in line.as_chunks_mut::<3>().0.iter_mut().zip(pixels) {
                *slot = [pixel[0], pixel[1], pixel[2]];
            }
        }
        Colour::Rgba => line.copy_from_slice(pixels.as_flattened()),
    }
}

/// The resolution fields a density is written as: the unit, and the
/// figures across and down. No density is stated as square pixels with no
/// unit, which is what it means.
fn resolution(density: Option<Density>) -> (u16, (u32, u32), (u32, u32)) {
    let Some(density) = density else {
        return (RESOLUTION_NONE, (1, 1), (1, 1));
    };
    let (unit, stated) = match density.unit() {
        DensityUnit::Aspect => (RESOLUTION_NONE, Some(density)),
        DensityUnit::Inch => (RESOLUTION_INCH, Some(density)),
        DensityUnit::Centimetre => (RESOLUTION_CENTIMETRE, Some(density)),
        DensityUnit::Metre => (
            RESOLUTION_CENTIMETRE,
            density.exact_in(DensityUnit::Centimetre),
        ),
    };
    match stated {
        Some(stated) => (unit, stated.across(), stated.down()),
        None => density
            .whole_in(DensityUnit::Centimetre)
            .map_or((RESOLUTION_NONE, (1, 1), (1, 1)), |(across, down)| {
                (RESOLUTION_CENTIMETRE, (across, 1), (down, 1))
            }),
    }
}

/// Which of a page's strip records an entry states.
#[derive(Copy, Clone)]
enum Which {
    Offsets,
    Counts,
}

/// One directory entry, its value inline or the offset of its values.
#[derive(Copy, Clone)]
struct Entry {
    tag: u16,
    kind: u16,
    count: u32,
    value: [u8; 4],
}

/// A page's directory, gathered in the order the format requires — every
/// tag ascending — with the values too long to sit inline written ahead of
/// it as they are added.
struct Directory<'w> {
    work: &'w mut Work,
}

impl<'w> Directory<'w> {
    fn new(work: &'w mut Work) -> Self {
        work.entries.clear();
        Self { work }
    }

    fn add(&mut self, tag: u16, kind: u16, count: u32, value: [u8; 4]) -> Result<(), EncodeError> {
        if !fallible::reserve(&mut self.work.entries, 1) {
            return Err(EncodeError::OutOfMemory);
        }
        self.work.entries.push(Entry {
            tag,
            kind,
            count,
            value,
        });
        Ok(())
    }

    fn short(&mut self, tag: u16, value: u16) -> Result<(), EncodeError> {
        let [low, high] = value.to_le_bytes();
        self.add(tag, SHORT, 1, [low, high, 0, 0])
    }

    fn long(&mut self, tag: u16, value: u32) -> Result<(), EncodeError> {
        self.add(tag, LONG, 1, value.to_le_bytes())
    }

    /// Where the next out-of-line values go: on a word boundary, as the
    /// format requires of every offset.
    fn values_at(out: &mut Output) -> Result<[u8; 4], EncodeError> {
        if out.len() % 2 == 1 {
            out.byte(0)?;
        }
        Ok(offset(out.len())?.to_le_bytes())
    }

    fn shorts(&mut self, out: &mut Output, tag: u16, values: &[u16]) -> Result<(), EncodeError> {
        let count = offset(values.len())?;
        let value = if values.len() <= 2 {
            let mut inline = [0; 4];
            for (slot, value) in inline.as_chunks_mut::<2>().0.iter_mut().zip(values) {
                *slot = value.to_le_bytes();
            }
            inline
        } else {
            let at = Self::values_at(out)?;
            for &value in values {
                out.le_u16(value)?;
            }
            at
        };
        self.add(tag, SHORT, count, value)
    }

    fn longs_from_work(
        &mut self,
        out: &mut Output,
        tag: u16,
        which: Which,
    ) -> Result<(), EncodeError> {
        let values = match which {
            Which::Offsets => &self.work.offsets,
            Which::Counts => &self.work.counts,
        };
        let count = offset(values.len())?;
        let value = match values.as_slice() {
            [one] => one.to_le_bytes(),
            many => {
                let at = Self::values_at(out)?;
                for &value in many {
                    out.le_u32(value)?;
                }
                at
            }
        };
        self.add(tag, LONG, count, value)
    }

    fn rational(
        &mut self,
        out: &mut Output,
        tag: u16,
        (numerator, denominator): (u32, u32),
    ) -> Result<(), EncodeError> {
        let at = Self::values_at(out)?;
        out.le_u32(numerator)?;
        out.le_u32(denominator)?;
        self.add(tag, RATIONAL, 1, at)
    }

    /// The colour map: every red, then every green, then every blue, each
    /// widened to sixteen bits exactly, for every index the depth names.
    fn colour_map(
        &mut self,
        out: &mut Output,
        bits: u32,
        palette: &[Rgba8],
    ) -> Result<(), EncodeError> {
        let entries = 1usize << bits;
        let at = Self::values_at(out)?;
        for channel in 0..3 {
            for index in 0..entries {
                let value = palette.get(index).map_or(0, |entry| entry[channel]);
                out.le_u16(u16::from(value) * 257)?;
            }
        }
        self.add(TAG_COLOUR_MAP, SHORT, offset(entries * 3)?, at)
    }

    /// Write the directory with no page after it, answering where it
    /// begins.
    fn finish(self, out: &mut Output) -> Result<usize, EncodeError> {
        if out.len() % 2 == 1 {
            out.byte(0)?;
        }
        let at = out.len();
        let entries = &self.work.entries;
        out.le_u16(u16::try_from(entries.len()).map_err(|_| EncodeError::TooLarge)?)?;
        for entry in entries {
            out.le_u16(entry.tag)?;
            out.le_u16(entry.kind)?;
            out.le_u32(entry.count)?;
            out.push(&entry.value)?;
        }
        out.le_u32(0)?;
        Ok(at)
    }
}

/// Pack one row as `PackBits` runs, each at most 128 bytes and none crossing
/// the row's end: equal bytes are repeated, two or more where a run begins and
/// three or more amid literal bytes, which a repeated pair would lengthen.
fn pack_bits(row: &[u8], out: &mut Output) -> Result<(), EncodeError> {
    const MOST: usize = 128;
    let mut at = 0;
    while at < row.len() {
        let byte = row[at];
        let run = row[at..]
            .iter()
            .take(MOST)
            .take_while(|&&next| next == byte)
            .count();
        if run >= 2 {
            out.push(&[
                u8::try_from(257 - run).map_err(|_| EncodeError::TooLarge)?,
                byte,
            ])?;
            at += run;
            continue;
        }
        let start = at;
        while at < row.len() && at - start < MOST {
            if row
                .get(at..at + 3)
                .is_some_and(|three| three[0] == three[1] && three[1] == three[2])
            {
                break;
            }
            at += 1;
        }
        out.byte(u8::try_from(at - start - 1).map_err(|_| EncodeError::TooLarge)?)?;
        out.push(&row[start..at])?;
    }
    Ok(())
}

/// Codes packed most significant bit first into a flat run of bytes.
struct MsbCodes<'a> {
    out: &'a mut Output,
    bits: u32,
    held: u32,
}

impl<'a> MsbCodes<'a> {
    fn new(out: &'a mut Output) -> Self {
        Self {
            out,
            bits: 0,
            held: 0,
        }
    }

    /// Write the bits still held, padded to a byte.
    fn finish(self) -> Result<(), EncodeError> {
        if self.held == 0 {
            return Ok(());
        }
        self.out
            .byte((self.bits << (8 - self.held)).to_le_bytes()[0])
    }
}

impl CodeSink for MsbCodes<'_> {
    fn put(&mut self, code: u16, width: u32) -> Result<(), EncodeError> {
        self.bits = (self.bits << width) | u32::from(code);
        self.held += width;
        while self.held >= 8 {
            self.held -= 8;
            self.out.byte((self.bits >> self.held).to_le_bytes()[0])?;
        }
        self.bits &= (1 << self.held) - 1;
        Ok(())
    }
}

#[cfg(test)]
#[path = "tiff_encode_tests.rs"]
mod tests;
