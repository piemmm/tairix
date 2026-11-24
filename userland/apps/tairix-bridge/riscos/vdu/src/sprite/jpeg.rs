//! SpriteExtend's JPEG SWIs (chunk `&49980`), as RISC OS 5 answers them
//! (held to it by the recorded corpus):
//!
//! - `JPEG_Info` and `JPEG_FileInfo` read a JPEG's frame header: its raster
//!   as stored, whether it is grey, its process where asked, and its density,
//!   a JFIF unit's dots per inch, or 90 times a pixel aspect where it states
//!   only that or nothing. A transformed plot is never offered, and the
//!   workspace a plot needs is RISC OS 5's figure for the width;
//! - the plots decode the raster as stored through `lib/image` and draw it as
//!   a deep sprite is drawn, scaled; on a palettised destination each colour
//!   is found in the destination's 32K table. A transformed plot is made only
//!   where its transform is a scaling, as RISC OS 5's SpriteExtend makes it;
//! - with `JPEG_PDriverIntercept`'s bit 0 set, the plots pass to
//!   `PDriver_JPEGSWI` with the JPEG SWI's offset in R8.
//!
//! RISC OS 5's own decoder dithers and rounds as it does; the pixels drawn
//! here are `lib/image`'s, and dithering is not offered.

use alloc::vec::Vec;

use tairix_image::{
    jpeg_decode_stored, jpeg_frame, DecodeError, DecodeLimits, DensityUnit, JpegFrame, RasterImage,
    STANDARD_SPRITE_DPI,
};
use tairix_riscos_arm::mmu::PAGE_SIZE;
use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::module::SWI_OUT_OF_RANGE;
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};
use tairix_riscos_kernel::swi::jpeg::PARALLELOGRAM;
use tairix_riscos_kernel::swi::os::{file as reason, FILE, SWI_NUMBER_FROM_STRING};
use tairix_riscos_kernel::swi::X;

use super::extend::{State, SPRITE_EXTEND};
use super::paint::{self, PlotFlags, Scale};
use super::{NO_MEMORY, RESERVED_FLAGS};

pub(crate) use tairix_riscos_kernel::swi::chunk::SPRITE_EXTEND as CHUNK;
/// `JPEG_PlotTransformed`'s place in [`CHUNK`], which other modules call.
const PLOT_TRANSFORMED: u32 = tairix_riscos_kernel::swi::jpeg::PLOT_TRANSFORMED - CHUNK;
/// The names of SpriteExtend's JPEG SWIs, in [`CHUNK`].
pub(crate) const NAMES: &[&str] = &[
    "Info",
    "FileInfo",
    "PlotScaled",
    "PlotFileScaled",
    "PlotTransformed",
    "PlotFileTransformed",
    "PDriverIntercept",
];

/// "Not valid JPEG data".
pub(crate) const BAD_JPEG: Error = Error {
    number: 0x71A,
    token: "SpriteBadJPEG",
};
/// "SpriteExtend plots a JPEG transformed only by a scaling".
pub(crate) const NOT_SCALING: Error = Error {
    number: 0x71B,
    token: "SpriteJPEGNotScaling",
};

/// `JPEG_Info`'s and `JPEG_FileInfo`'s entry flags: the dimensions and the
/// process asked for; any other is reserved.
const ASK_DIMENSIONS: u32 = 1;
const ASK_PROCESS: u32 = 2;
/// Their exit flags.
const GREY: u32 = 1;
const NO_TRANSFORMED_PLOTS: u32 = 2;
const DENSITY_RATIO: u32 = 4;
const PROCESS_SHIFT: u32 = 3;
/// RISC OS 5's workspace for a plot: a base, and a part for each 32 pixels
/// across.
const WORKSPACE_BASE: u32 = 0x8000;
const WORKSPACE_PER_STRIP: u32 = 0x1000;
const WORKSPACE_STRIP: u32 = 32;
/// The most of a JPEG in memory `JPEG_Info` reads for its header: RISC OS 5
/// reads past the length it is given, so the header is read without it.
const HEADER_MOST: u32 = 0x1_0000;
/// The plot flags' translucency, `JPEG_PlotScaled`'s and the transformed
/// plots', and their colour mapping bits.
const SCALED_TRANSLUCENCY_SHIFT: u32 = 4;
const SCALED_MAPPING: u32 = 8;
const TRANSFORMED_TRANSLUCENCY_SHIFT: u32 = 5;
const TRANSFORMED_MAPPING: u32 = 0x10;
const TRANSLUCENCY: u32 = 0xFF;
/// `JPEG_PDriverIntercept`'s bit passing the plots on.
const PASS_ON: u32 = 1;
/// The SWI the plots pass to.
const PASS_TO: &[u8] = b"PDriver_JPEGSWI";
/// The pictures a plot decodes at most: far past any screen, within what one
/// session's memory holds.
const LIMITS: DecodeLimits = DecodeLimits::new(1 << 14, 1 << 14, 1 << 24, 1 << 27);

/// SpriteExtend's JPEG SWIs.
pub(crate) fn swi(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    let plotting = matches!(offset, 2..=5);
    if plotting && intercept(os) & PASS_ON != 0 {
        return pass_on(os, offset, regs);
    }
    match offset {
        0 => {
            if regs.r[0] & !(ASK_DIMENSIONS | ASK_PROCESS) != 0 {
                return Err(os.error(RESERVED_FLAGS));
            }
            let header = header_bytes(os, regs.r[1])?;
            info(os, &header, regs)
        }
        1 => {
            if regs.r[0] & !(ASK_DIMENSIONS | ASK_PROCESS) != 0 {
                return Err(os.error(RESERVED_FLAGS));
            }
            let bytes = file_bytes(os, regs.r[1])?;
            info(os, &bytes, regs)
        }
        2 => {
            let bytes = os.read_vec(regs.r[0], regs.r[4] as usize)?;
            let [_, x, y, scale, _, flags, descriptor, ..] = regs.r;
            plot_scaled(
                os,
                &bytes,
                (x, y),
                scale,
                Picture::of(flags, SCALED_TRANSLUCENCY_SHIFT, SCALED_MAPPING, descriptor),
            )
        }
        3 => {
            let bytes = file_bytes(os, regs.r[0])?;
            let [_, x, y, scale, flags, descriptor, ..] = regs.r;
            plot_scaled(
                os,
                &bytes,
                (x, y),
                scale,
                Picture::of(flags, SCALED_TRANSLUCENCY_SHIFT, SCALED_MAPPING, descriptor),
            )
        }
        PLOT_TRANSFORMED => {
            let bytes = os.read_vec(regs.r[0], regs.r[3] as usize)?;
            let [_, flags, shape, _, descriptor, ..] = regs.r;
            plot_transformed(os, &bytes, flags, shape, descriptor)
        }
        5 => {
            let bytes = file_bytes(os, regs.r[0])?;
            let [_, flags, shape, descriptor, ..] = regs.r;
            plot_transformed(os, &bytes, flags, shape, descriptor)
        }
        6 => {
            let was = intercept(os);
            if let Some(state) = os.extension::<State>() {
                state.intercept = regs.r[0];
            }
            regs.r[0] = was;
            Ok(())
        }
        _ => Err(os.error_with(SWI_OUT_OF_RANGE, &[SPRITE_EXTEND.title.as_bytes()])),
    }
}

/// `JPEG_PDriverIntercept`'s flags now.
fn intercept(os: &mut Os<'_>) -> u32 {
    os.extension::<State>().map_or(0, |state| state.intercept)
}

/// A plot passed to `PDriver_JPEGSWI`, the JPEG SWI's offset in R8.
fn pass_on(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    let number = os.with_text(PASS_TO, 0, |os, at| {
        let mut lookup = Regs::new([at, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        os.perform(X | SWI_NUMBER_FROM_STRING, &mut lookup)?;
        Ok(lookup.r[0])
    })?;
    regs.r[8] = offset;
    os.perform(X | number, regs)
}

/// `JPEG_Info`'s and `JPEG_FileInfo`'s answers for `bytes`: R0 its flags,
/// R2 and R3 its raster where asked, R4 and R5 its density, R6 a plot's
/// workspace.
fn info(os: &mut Os<'_>, bytes: &[u8], regs: &mut Regs) -> Result<(), Fail> {
    let frame = jpeg_frame(bytes).map_err(|error| refused(os, &error))?;
    let asked = regs.r[0];
    let (density, ratio) = density(&frame);
    let mut flags = NO_TRANSFORMED_PLOTS;
    if frame.components == 1 {
        flags |= GREY;
    }
    if ratio {
        flags |= DENSITY_RATIO;
    }
    if asked & ASK_PROCESS != 0 {
        flags |= u32::from(frame.process.number()) << PROCESS_SHIFT;
    }
    regs.r[0] = flags;
    if asked & ASK_DIMENSIONS != 0 {
        (regs.r[2], regs.r[3]) = frame.stored;
    }
    (regs.r[4], regs.r[5]) = density;
    regs.r[6] = WORKSPACE_BASE + frame.stored.0.div_ceil(WORKSPACE_STRIP) * WORKSPACE_PER_STRIP;
    Ok(())
}

/// The density `frame` states, and whether it is a pixel aspect: dots per
/// inch, a centimetre's made an inch's and cut down; else 90 times the
/// aspect in lowest terms, or 90 each way where it states none.
fn density(frame: &JpegFrame) -> ((u32, u32), bool) {
    let Some(density) = frame.density else {
        return ((STANDARD_SPRITE_DPI, STANDARD_SPRITE_DPI), true);
    };
    let whole = |(n, d): (u32, u32)| n / d.max(1);
    let (across, down) = (whole(density.across()), whole(density.down()));
    match density.unit() {
        DensityUnit::Inch => ((across, down), false),
        DensityUnit::Centimetre => ((inches(across, 100), inches(down, 100)), false),
        DensityUnit::Metre => ((inches(across, 10_000), inches(down, 10_000)), false),
        DensityUnit::Aspect => {
            let (across, down) = density.shape().unwrap_or((1, 1));
            let scale = |v: u32| v.saturating_mul(STANDARD_SPRITE_DPI);
            ((scale(across), scale(down)), true)
        }
    }
}

/// `per` dots to a unit of `hundredths` hundredths of a centimetre, made
/// dots per inch and cut down.
fn inches(per: u32, hundredths: u32) -> u32 {
    u32::try_from(u64::from(per) * 254 / u64::from(hundredths)).unwrap_or(u32::MAX)
}

/// A plot's picture options: translucency and the colour mapping.
#[derive(Clone, Copy)]
struct Picture {
    translucency: u32,
    mapping: Option<u32>,
}

impl Picture {
    fn of(flags: u32, translucency_shift: u32, mapping_bit: u32, descriptor: u32) -> Self {
        Self {
            translucency: (flags >> translucency_shift) & TRANSLUCENCY,
            mapping: (flags & mapping_bit != 0).then_some(descriptor),
        }
    }
}

/// The JPEG `bytes` plotted from `(x, y)` in OS units, its pixels scaled by
/// the factors at `scale`.
fn plot_scaled(
    os: &mut Os<'_>,
    bytes: &[u8],
    (x, y): (u32, u32),
    scale: u32,
    picture: Picture,
) -> Result<(), Fail> {
    let at = paint::at(os, x, y)?;
    let Some(scale) = Scale::read(os, scale)? else {
        return Ok(());
    };
    let frame = drawn_frame(os, bytes)?;
    plot(os, bytes, &frame, at, scale, picture)
}

/// The frame of the JPEG `bytes` a plot draws, refused as its decode would
/// refuse it where it holds no pixels: a plot is sized before it decodes.
fn drawn_frame(os: &mut Os<'_>, bytes: &[u8]) -> Result<JpegFrame, Fail> {
    let frame = jpeg_frame(bytes).map_err(|error| refused(os, &error))?;
    if frame.stored.0 == 0 || frame.stored.1 == 0 {
        return Err(refused(os, &DecodeError::ZeroDimension));
    }
    Ok(frame)
}

/// The JPEG `bytes` of `frame` decoded and plotted from pixel `at`, scaled.
fn plot(
    os: &mut Os<'_>,
    bytes: &[u8],
    frame: &JpegFrame,
    at: crate::raster::Point,
    scale: Scale,
    picture: Picture,
) -> Result<(), Fail> {
    let flags = PlotFlags(picture.translucency << PlotFlags::TRANSLUCENCY_SHIFT);
    let decode = |os: &mut Os<'_>| {
        jpeg_decode_stored(bytes, &LIMITS)
            .map(RasterImage::into_pixels)
            .map_err(|error| refused(os, &error))
    };
    paint::put_picture(os, frame.stored, decode, at, scale, flags, picture.mapping)
}

/// The JPEG `bytes` through R2's matrix or parallelogram, plotted where
/// that is a scaling.
fn plot_transformed(
    os: &mut Os<'_>,
    bytes: &[u8],
    flags: u32,
    shape: u32,
    descriptor: u32,
) -> Result<(), Fail> {
    let frame = drawn_frame(os, bytes)?;
    let picture = Picture::of(
        flags,
        TRANSFORMED_TRANSLUCENCY_SHIFT,
        TRANSFORMED_MAPPING,
        descriptor,
    );
    let Some((at, scale)) = paint::scaling_of(os, frame.stored, flags & PARALLELOGRAM != 0, shape)?
    else {
        return Err(os.error(NOT_SCALING));
    };
    plot(os, bytes, &frame, at, scale, picture)
}

/// The error a JPEG `lib/image` refuses gives: memory where the machine is
/// short of it, else not JPEG data.
fn refused(os: &mut Os<'_>, error: &DecodeError) -> Fail {
    match error {
        DecodeError::OutOfMemory
        | DecodeError::WidthExceedsLimit
        | DecodeError::HeightExceedsLimit
        | DecodeError::PixelCountExceedsLimit
        | DecodeError::DimensionsOverflow => os.error(NO_MEMORY),
        _ => os.error(BAD_JPEG),
    }
}

/// The JPEG at `at` as far as its header runs, at most [`HEADER_MOST`]
/// bytes: read a page at a time while the header needs more, so memory
/// ending past it is never touched and memory ending within it aborts where
/// the header meets it, as RISC OS 5 reads it.
fn header_bytes(os: &mut Os<'_>, at: u32) -> Result<Vec<u8>, Fail> {
    let mut bytes = Vec::new();
    while bytes.len() < HEADER_MOST as usize {
        let here = at.wrapping_add(len32(bytes.len()));
        let to_page_end = PAGE_SIZE - here % PAGE_SIZE;
        let n = to_page_end.min(HEADER_MOST - len32(bytes.len()));
        os.read_onto(&mut bytes, here, n as usize)?;
        if !runs_on(&bytes) {
            break;
        }
    }
    Ok(bytes)
}

/// Whether a JPEG header begun in `bytes` runs on past them.
fn runs_on(bytes: &[u8]) -> bool {
    bytes.len() < 2
        || matches!(
            jpeg_frame(bytes),
            Err(DecodeError::JpegMarkerTruncated | DecodeError::JpegSegmentLengthExceedsInput)
        )
}

/// File `name` whole, read through a block of the RMA.
fn file_bytes(os: &mut Os<'_>, name: u32) -> Result<Vec<u8>, Fail> {
    let length = super::file::file_length(os, name)?;
    os.read_through_rma(length, |os, block| {
        let mut load = Regs::new([reason::LOAD, name, block, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        os.perform(X | FILE, &mut load)
    })
}

#[cfg(test)]
mod tests;
