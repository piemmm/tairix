//! A BMP encoder: a palette picture at 1, 4 or 8 bits, opaque colour at 24
//! bits, and anything with transparency at 32 bits under a V4 header whose
//! alpha mask says so. Rows are written bottom up and uncompressed.
//!
//! A BMP palette holds no opacity, so a palette picture any of whose shown
//! pixels is translucent is written as colour rather than losing it.

use alloc::vec::Vec;

use crate::bmp::{
    stride, BI_BITFIELDS, BI_RGB, CALIBRATION, FILE_HEADER_LEN, INFO_HEADER_LEN, LCS_SRGB, MAGIC,
    V4_HEADER_LEN,
};
use crate::density::DensityUnit;
use crate::encode::{
    indexed_translucent, pack, palette_fits, scratch, survey, EncodeError, Output, RowBuffers,
};
use crate::picture::{flatten_row, IndexDepth, PictureKind, PictureSource, Rgba8};
use crate::RGBA_BYTES;

/// The red, green, blue and alpha masks of a 32-bit pixel, in the order the
/// header states them.
const MASKS: [u32; RGBA_BYTES] = [0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000];

/// How the pixels are written.
enum Plan<'a> {
    Indexed { bits: u32, palette: &'a [Rgba8] },
    Rgb,
    Rgba,
}

impl Plan<'_> {
    const fn bits(&self) -> u32 {
        match self {
            Self::Indexed { bits, .. } => *bits,
            Self::Rgb => 24,
            Self::Rgba => 32,
        }
    }
}

pub(crate) fn encode(source: &dyn PictureSource) -> Result<Vec<u8>, EncodeError> {
    let (width, height) = (source.width(), source.height());
    let (Ok(across), Ok(down)) = (i32::try_from(width), i32::try_from(height)) else {
        return Err(EncodeError::TooLarge);
    };
    if across == 0 || down == 0 {
        return Err(EncodeError::TooLarge);
    }
    let kind = source.kind();
    let mut rows = RowBuffers::for_source(source)?;
    let plan = plan(source, kind, &mut rows)?;
    let row_len = stride(width, plan.bits()).map_err(|_| EncodeError::TooLarge)?;
    let image_len = row_len
        .checked_mul(usize::try_from(height).map_err(|_| EncodeError::TooLarge)?)
        .ok_or(EncodeError::TooLarge)?;
    let (header_len, colours) = match &plan {
        Plan::Indexed { palette, .. } => (INFO_HEADER_LEN, palette.len()),
        Plan::Rgb => (INFO_HEADER_LEN, 0),
        Plan::Rgba => (V4_HEADER_LEN, 0),
    };
    let offset = FILE_HEADER_LEN + header_len as usize + colours * RGBA_BYTES;
    let field = |value: usize| u32::try_from(value).map_err(|_| EncodeError::TooLarge);
    let file_len = field(offset.checked_add(image_len).ok_or(EncodeError::TooLarge)?)?;
    let (x_density, y_density) = source
        .density()
        .and_then(|density| density.whole_in(DensityUnit::Metre))
        .and_then(|(x, y)| Some((i32::try_from(x).ok()?, i32::try_from(y).ok()?)))
        .unwrap_or((0, 0));

    let mut out = Output::new();
    out.push(&MAGIC)?;
    out.le_u32(file_len)?;
    out.le_u32(0)?;
    out.le_u32(field(offset)?)?;
    out.le_u32(header_len)?;
    out.push(&across.to_le_bytes())?;
    out.push(&down.to_le_bytes())?;
    out.le_u16(1)?;
    out.le_u16(u16::try_from(plan.bits()).map_err(|_| EncodeError::TooLarge)?)?;
    out.le_u32(if matches!(plan, Plan::Rgba) {
        BI_BITFIELDS
    } else {
        BI_RGB
    })?;
    out.le_u32(field(image_len)?)?;
    out.push(&x_density.to_le_bytes())?;
    out.push(&y_density.to_le_bytes())?;
    out.le_u32(field(colours)?)?;
    out.le_u32(0)?;
    if matches!(plan, Plan::Rgba) {
        for mask in MASKS {
            out.le_u32(mask)?;
        }
        out.le_u32(LCS_SRGB)?;
        // The endpoints and gamma an sRGB header leaves zero.
        out.push(&[0; CALIBRATION.end - CALIBRATION.start])?;
    }
    if let Plan::Indexed { palette, .. } = &plan {
        for entry in *palette {
            out.push(&[entry[2], entry[1], entry[0], 0])?;
        }
    }
    let width = usize::try_from(width).map_err(|_| EncodeError::TooLarge)?;
    let mut line = scratch(row_len)?;
    let mut rgba = if matches!(plan, Plan::Indexed { .. }) {
        Vec::new()
    } else {
        scratch(width * RGBA_BYTES)?
    };
    for y in (0..height).rev() {
        rows.read(source, y);
        match &plan {
            Plan::Indexed { bits, .. } => pack(rows.samples.iter().copied(), *bits, &mut line),
            Plan::Rgb | Plan::Rgba => {
                let pixels = if matches!(kind, PictureKind::Rgba) {
                    &rows.samples
                } else {
                    flatten_row(kind, &rows.samples, &rows.mask, &mut rgba);
                    &rgba
                };
                let alpha = matches!(plan, Plan::Rgba);
                let per = if alpha { 4 } else { 3 };
                for (pixel, slot) in pixels
                    .as_chunks::<RGBA_BYTES>()
                    .0
                    .iter()
                    .zip(line.chunks_exact_mut(per))
                {
                    slot[..3].copy_from_slice(&[pixel[2], pixel[1], pixel[0]]);
                    if alpha {
                        slot[3] = pixel[3];
                    }
                }
            }
        }
        out.push(&line)?;
    }
    Ok(out.into_bytes())
}

/// Read the picture once to choose how it is written.
fn plan<'a>(
    source: &'a dyn PictureSource,
    kind: PictureKind<'a>,
    rows: &mut RowBuffers,
) -> Result<Plan<'a>, EncodeError> {
    match kind {
        PictureKind::Indexed {
            depth,
            palette,
            masked,
        } => {
            palette_fits(depth, palette)?;
            if indexed_translucent(source, rows, palette, masked)? {
                return Ok(Plan::Rgba);
            }
            // A BMP has no two-bit depth, so two-bit indices take four.
            let bits = match depth {
                IndexDepth::One => 1,
                IndexDepth::Two | IndexDepth::Four => 4,
                IndexDepth::Eight => 8,
            };
            Ok(Plan::Indexed { bits, palette })
        }
        PictureKind::Rgba => Ok(if survey(source, rows, &mut []).opaque {
            Plan::Rgb
        } else {
            Plan::Rgba
        }),
    }
}

#[cfg(test)]
#[path = "bmp_encode_tests.rs"]
mod tests;
