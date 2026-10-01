//! A lossless PNG encoder (W3C PNG specification) that writes a picture in
//! the smallest colour type holding it exactly.
//!
//! An indexed picture keeps its palette and every index: a binary mask
//! becomes one fully transparent entry — a new one while the palette has
//! room, else one no visible pixel selects — and only a partial mask, or a
//! full palette every entry of which is on show, is written as RGBA. A
//! truecolour picture drops an alpha channel it never uses and colour it
//! never has, down to the shallowest grey depth its levels are exact at. It
//! is never turned into palette indices: a truecolour document should reopen
//! as one.
//!
//! Rows of whole-byte pixels are filtered by the specification's own
//! heuristic, the filter whose output bytes sum smallest as signed values;
//! palette and sub-byte grey rows are left unfiltered, which is what the
//! specification recommends for them.

use alloc::vec::Vec;

use tairix_compress::deflate::Flush;
use tairix_compress::zlib::Encoder;
use tairix_util::fallible;

use crate::encode::{indices_fit, palette_fits, scratch, EncodeError, Output, RowBuffers};
use crate::picture::{flatten_row, IndexDepth, PictureKind, PictureSource, Rgba8};
use crate::png::{self, ColourType, IDAT, IEND, IHDR, PLTE, TRNS};
use crate::{PNG_SIGNATURE, RGBA_BYTES};

/// Largest width or height a PNG header's four-byte fields may state.
const MAX_SIDE: u32 = (1 << 31) - 1;

/// Compressed bytes one `IDAT` chunk carries: large enough that the framing
/// is noise, small enough for a streaming reader's buffer.
const IDAT_CHUNK: usize = 64 * 1024;

/// The colour type a picture is written as.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Plan {
    /// Palette indices; masked pixels take `transparent`.
    Indexed {
        depth: IndexDepth,
        palette: Vec<Rgba8>,
        transparent: Option<u8>,
    },
    /// Opaque grey levels, exact at `bits` bits.
    Grey {
        bits: u32,
    },
    GreyAlpha,
    Rgb,
    Rgba,
}

impl Plan {
    const fn colour_type(&self) -> ColourType {
        match self {
            Self::Grey { .. } => ColourType::Grey,
            Self::Rgb => ColourType::Truecolour,
            Self::Indexed { .. } => ColourType::Indexed,
            Self::GreyAlpha => ColourType::GreyAlpha,
            Self::Rgba => ColourType::Rgba,
        }
    }

    const fn bit_depth(&self) -> u32 {
        match self {
            Self::Indexed { depth, .. } => depth.bits(),
            Self::Grey { bits } => *bits,
            Self::GreyAlpha | Self::Rgb | Self::Rgba => 8,
        }
    }

    const fn bits_per_pixel(&self) -> u32 {
        self.colour_type().channels() * self.bit_depth()
    }

    /// Bytes per pixel the filters step by, for a plan whose rows are
    /// filtered at all: palette and sub-byte rows are not.
    fn filter_step(&self) -> Option<usize> {
        let bits = self.bits_per_pixel();
        (!matches!(self, Self::Indexed { .. }) && bits >= 8).then(|| png::filter_bpp(bits))
    }
}

pub(crate) fn encode(source: &dyn PictureSource) -> Result<Vec<u8>, EncodeError> {
    let (width, height) = (source.width(), source.height());
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(EncodeError::TooLarge);
    }
    let mut rows = RowBuffers::for_source(source)?;
    let plan = plan(source, &mut rows)?;

    let mut out = Output::new();
    out.push(&PNG_SIGNATURE)?;
    let mut header = [0u8; 13];
    header[0..4].copy_from_slice(&width.to_be_bytes());
    header[4..8].copy_from_slice(&height.to_be_bytes());
    header[8] = u8::try_from(plan.bit_depth()).map_err(|_| EncodeError::TooLarge)?;
    header[9] = plan.colour_type().code();
    chunk(&mut out, IHDR, &header)?;
    if let Plan::Indexed { palette, .. } = &plan {
        let mut colours = [[0u8; 3]; 256];
        let mut alphas = [0u8; 256];
        let held = colours.iter_mut().zip(&mut alphas).zip(palette);
        for ((colour, alpha), entry) in held {
            *colour = [entry[0], entry[1], entry[2]];
            *alpha = entry[3];
        }
        let count = palette.len().min(colours.len());
        chunk(&mut out, PLTE, colours[..count].as_flattened())?;
        if let Some(last) = alphas[..count].iter().rposition(|&alpha| alpha != u8::MAX) {
            chunk(&mut out, TRNS, &alphas[..=last])?;
        }
    }
    write_image(&mut out, source, &mut rows, &plan)?;
    chunk(&mut out, IEND, &[])?;
    Ok(out.into_bytes())
}

/// Append one chunk: its length, type, payload and CRC.
fn chunk(out: &mut Output, kind: [u8; 4], data: &[u8]) -> Result<(), EncodeError> {
    out.be_u32(u32::try_from(data.len()).map_err(|_| EncodeError::TooLarge)?)?;
    out.push(&kind)?;
    out.push(data)?;
    out.be_u32(png::chunk_crc(kind, data))
}

/// Read the whole picture once and choose the colour type it is written as.
fn plan(source: &dyn PictureSource, rows: &mut RowBuffers) -> Result<Plan, EncodeError> {
    match source.kind() {
        PictureKind::Indexed {
            depth,
            palette,
            masked,
        } => {
            palette_fits(depth, palette)?;
            plan_indexed(source, rows, palette, masked)
        }
        PictureKind::Rgba => Ok(plan_rgba(source, rows)),
    }
}

fn plan_indexed(
    source: &dyn PictureSource,
    rows: &mut RowBuffers,
    palette: &[Rgba8],
    masked: bool,
) -> Result<Plan, EncodeError> {
    let mut shown = [false; 256];
    let (mut hidden, mut partial) = (false, false);
    for y in 0..source.height() {
        rows.read(source, y);
        indices_fit(&rows.samples, palette.len())?;
        for (x, &index) in rows.samples.iter().enumerate() {
            match rows.mask.get(x).copied().filter(|_| masked) {
                Some(0) => hidden = true,
                Some(u8::MAX) | None => shown[usize::from(index)] = true,
                Some(_) => partial = true,
            }
        }
    }
    if partial {
        return Ok(Plan::Rgba);
    }
    let mut palette = fallible::collected(palette.len() + 1, palette.iter().copied())
        .ok_or(EncodeError::OutOfMemory)?;
    let transparent = if hidden {
        let slot = if palette.len() < IndexDepth::Eight.colours() {
            palette.push([0; 4]);
            palette.len() - 1
        } else if let Some(unshown) = shown.iter().position(|&shown| !shown) {
            palette[unshown] = [0; 4];
            unshown
        } else {
            return Ok(Plan::Rgba);
        };
        Some(u8::try_from(slot).map_err(|_| EncodeError::InvalidPalette)?)
    } else {
        None
    };
    let depth = IndexDepth::holding(palette.len()).ok_or(EncodeError::InvalidPalette)?;
    Ok(Plan::Indexed {
        depth,
        palette,
        transparent,
    })
}

fn plan_rgba(source: &dyn PictureSource, rows: &mut RowBuffers) -> Plan {
    let (mut opaque, mut grey) = (true, true);
    // Whether every grey level is exact at one, two and four bits.
    let mut exact = [true; 3];
    for y in 0..source.height() {
        rows.read(source, y);
        for pixel in rows.samples.as_chunks::<RGBA_BYTES>().0 {
            opaque &= pixel[3] == u8::MAX;
            if grey && (pixel[0] != pixel[1] || pixel[1] != pixel[2]) {
                grey = false;
            }
            if grey {
                let level = pixel[0];
                exact[0] &= level == 0 || level == u8::MAX;
                exact[1] &= level % 85 == 0;
                exact[2] &= level % 17 == 0;
            }
        }
        if !opaque && !grey {
            return Plan::Rgba;
        }
    }
    match (grey, opaque) {
        (true, true) => Plan::Grey {
            bits: [1, 2, 4]
                .into_iter()
                .zip(exact)
                .find_map(|(bits, exact)| exact.then_some(bits))
                .unwrap_or(8),
        },
        (true, false) => Plan::GreyAlpha,
        (false, true) => Plan::Rgb,
        (false, false) => Plan::Rgba,
    }
}

/// Pack `values`, `bits` wide each, most significant first, into `out`.
fn pack(values: impl Iterator<Item = u8>, bits: u32, out: &mut [u8]) {
    out.fill(0);
    // Counted in `u64`: a row is as wide as `MAX_SIDE`, whose bits a `u32`
    // does not hold.
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

/// Write one row of `plan` from the buffers `source`'s row was read into.
fn scanline(
    plan: &Plan,
    kind: PictureKind<'_>,
    rows: &RowBuffers,
    rgba: &mut [u8],
    line: &mut [u8],
) {
    let samples = &rows.samples;
    match plan {
        Plan::Indexed {
            depth, transparent, ..
        } => {
            let values = samples.iter().enumerate().map(|(x, &index)| {
                match (transparent, rows.mask.get(x)) {
                    (Some(slot), Some(0)) => *slot,
                    _ => index,
                }
            });
            pack(values, depth.bits(), line);
        }
        Plan::Grey { bits } => {
            let levels = samples
                .as_chunks::<RGBA_BYTES>()
                .0
                .iter()
                .map(|pixel| pixel[0] >> (8 - bits));
            pack(levels, *bits, line);
        }
        Plan::GreyAlpha => {
            for (dst, pixel) in line
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .zip(samples.as_chunks::<4>().0)
            {
                *dst = [pixel[0], pixel[3]];
            }
        }
        Plan::Rgb => {
            for (dst, pixel) in line
                .as_chunks_mut::<3>()
                .0
                .iter_mut()
                .zip(samples.as_chunks::<4>().0)
            {
                *dst = [pixel[0], pixel[1], pixel[2]];
            }
        }
        Plan::Rgba => match kind {
            PictureKind::Rgba => line.copy_from_slice(samples),
            PictureKind::Indexed { .. } => {
                flatten_row(kind, samples, &rows.mask, rgba);
                line.copy_from_slice(rgba);
            }
        },
    }
}

/// Apply filter `kind` to `line` against `prior`, `step` bytes per pixel,
/// into `out`, answering the sum of its bytes read as signed values.
fn filter(kind: u8, line: &[u8], prior: &[u8], step: usize, out: &mut [u8]) -> u64 {
    let mut sum = 0u64;
    for (i, ((slot, &x), &b)) in out.iter_mut().zip(line).zip(prior).enumerate() {
        let a = if i >= step { line[i - step] } else { 0 };
        let c = if i >= step { prior[i - step] } else { 0 };
        *slot = x.wrapping_sub(png::predict(kind, a, b, c).unwrap_or(0));
        sum += u64::from(slot.cast_signed().unsigned_abs());
    }
    sum
}

/// Filter, compress and frame every row as `IDAT` chunks.
fn write_image(
    out: &mut Output,
    source: &dyn PictureSource,
    rows: &mut RowBuffers,
    plan: &Plan,
) -> Result<(), EncodeError> {
    let width = u64::from(source.width());
    let line_len = usize::try_from((width * u64::from(plan.bits_per_pixel())).div_ceil(8))
        .map_err(|_| EncodeError::TooLarge)?;
    let rgba_len = usize::try_from(width * RGBA_BYTES as u64).map_err(|_| EncodeError::TooLarge)?;
    let mut line = scratch(line_len)?;
    let mut prior = scratch(line_len)?;
    // The filter byte leads each row; the best trial so far and the one
    // being weighed are swapped rather than copied.
    let mut best = scratch(line_len + 1)?;
    let mut trial = scratch(line_len + 1)?;
    let mut rgba = if matches!(plan, Plan::Rgba) {
        scratch(rgba_len)?
    } else {
        Vec::new()
    };
    let mut encoders = Vec::new();
    if !fallible::reserve(&mut encoders, 1) {
        return Err(EncodeError::OutOfMemory);
    }
    encoders.push(Encoder::new());
    let encoder = &mut encoders[0];
    let mut pending = Pending {
        bytes: Vec::new(),
        filled: 0,
    };
    let kind = source.kind();
    for y in 0..source.height() {
        rows.read(source, y);
        scanline(plan, kind, rows, &mut rgba, &mut line);
        if let Some(step) = plan.filter_step() {
            let mut least = u64::MAX;
            for filter_type in 0..=4u8 {
                let sum = filter(filter_type, &line, &prior, step, &mut trial[1..]);
                if sum < least {
                    least = sum;
                    trial[0] = filter_type;
                    core::mem::swap(&mut best, &mut trial);
                }
            }
        } else {
            best[0] = 0;
            best[1..].copy_from_slice(&line);
        }
        pending.deflate(encoder, &best, Flush::None)?;
        pending.drain(out, IDAT_CHUNK)?;
        core::mem::swap(&mut line, &mut prior);
    }
    pending.deflate(encoder, &[], Flush::Finish)?;
    pending.drain(out, 1)
}

/// Compressed bytes waiting to be framed as `IDAT` chunks. The buffer is
/// kept at its high-water length, so compressing into it zero-fills each
/// byte once rather than once a row.
struct Pending {
    bytes: Vec<u8>,
    filled: usize,
}

impl Pending {
    /// Compress `input` under `flush` straight onto the end of what waits.
    fn deflate(
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

    /// Frame whole `IDAT_CHUNK`s while at least `least` bytes wait, the last
    /// taking whatever is left.
    fn drain(&mut self, out: &mut Output, least: usize) -> Result<(), EncodeError> {
        let mut start = 0;
        while self.filled - start >= least.max(1) {
            let take = (self.filled - start).min(IDAT_CHUNK);
            if take < IDAT_CHUNK && least > 1 {
                break;
            }
            chunk(out, IDAT, &self.bytes[start..start + take])?;
            start += take;
        }
        self.bytes.copy_within(start..self.filled, 0);
        self.filled -= start;
        Ok(())
    }
}

#[cfg(test)]
#[path = "png_encode_tests.rs"]
mod tests;
