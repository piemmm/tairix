//! A GIF encoder: one frame of palette indices over a global colour table,
//! LZW coded.
//!
//! A GIF has one transparent colour and no partial transparency, so a pixel
//! is shown or not at half opacity. The transparent index is one the picture
//! already gives its clear pixels where no shown pixel uses it — which is how
//! a GIF read back states its own — else a new entry while the palette has
//! room, else an entry no shown pixel uses.

use alloc::vec::Vec;

use tairix_util::fallible;

use crate::encode::{
    indices_fit, palette_fits, scratch, EncodeError, GifOptions, Output, RowBuffers,
};
use crate::gif::{
    aspect_byte, interlaced_row, EXTENSION_INTRODUCER, GRAPHIC_CONTROL_LEN, IMAGE_SEPARATOR,
    LABEL_GRAPHIC_CONTROL, MAGIC, MIN_CODE_SIZE_MIN, TRAILER, VERSIONS,
};
use crate::lzw::{CodeSink, Coder, Widen, MAX_CODES};
use crate::picture::{masked_colour, IndexDepth, PictureKind, PictureSource, Rgba8};

/// The least opacity a pixel or an entry shows at.
const SHOWN_FROM: u8 = 128;

/// Bytes one data sub-block carries at most.
const SUB_BLOCK: usize = 255;

pub(crate) fn encode(
    source: &dyn PictureSource,
    options: GifOptions,
) -> Result<Vec<u8>, EncodeError> {
    let (width, height) = (source.width(), source.height());
    let (Ok(across), Ok(down)) = (u16::try_from(width), u16::try_from(height)) else {
        return Err(EncodeError::TooLarge);
    };
    if across == 0 || down == 0 {
        return Err(EncodeError::TooLarge);
    }
    let PictureKind::Indexed {
        depth,
        palette,
        masked,
    } = source.kind()
    else {
        return Err(EncodeError::NotIndexed);
    };
    palette_fits(depth, palette)?;
    let mut rows = RowBuffers::for_source(source)?;
    let plan = Plan::of(source, &mut rows, palette, masked)?;

    let mut out = Output::new();
    out.push(&MAGIC)?;
    out.push(&VERSIONS[1])?;
    let bits = table_bits(plan.palette.len());
    out.le_u16(across)?;
    out.le_u16(down)?;
    out.byte(0x80 | ((bits - 1) << 4) | (bits - 1))?;
    out.push(&[0, aspect_byte(source.density())])?;
    for at in 0..1usize << bits {
        let entry = plan.palette.get(at).copied().unwrap_or([0; 4]);
        out.push(&entry[..3])?;
    }
    if let Some(transparent) = plan.transparent {
        out.push(&[
            EXTENSION_INTRODUCER,
            LABEL_GRAPHIC_CONTROL,
            GRAPHIC_CONTROL_LEN,
            0x01,
            0,
            0,
            transparent,
            0,
        ])?;
    }
    out.byte(IMAGE_SEPARATOR)?;
    for field in [0, 0, across, down] {
        out.le_u16(field)?;
    }
    out.byte(if options.interlaced { 0x40 } else { 0 })?;
    let root_bits = bits.max(MIN_CODE_SIZE_MIN);
    out.byte(root_bits)?;
    let limit = u16::try_from(MAX_CODES).map_err(|_| EncodeError::TooLarge)?;
    let mut coder =
        Coder::new(u32::from(root_bits), Widen::WhenFull, limit).ok_or(EncodeError::OutOfMemory)?;
    let mut sink = SubBlocks::new(&mut out);
    coder.begin(&mut sink)?;
    let mut indices = scratch(usize::from(across))?;
    for stream_row in 0..height {
        let y = if options.interlaced {
            interlaced_row(stream_row, height)
        } else {
            stream_row
        };
        rows.read(source, y);
        plan.write_row(&rows, &mut indices);
        for &index in &indices {
            coder.push(index, &mut sink)?;
        }
    }
    coder.finish(&mut sink)?;
    sink.finish()?;
    out.byte(TRAILER)?;
    Ok(out.into_bytes())
}

/// The colour table's size, as the bits its indices take: at least one, as
/// the format's smallest table has two entries.
fn table_bits(colours: usize) -> u8 {
    let mut bits = 1;
    while bits < 8 && 1usize << bits < colours {
        bits += 1;
    }
    bits
}

/// The palette written and the transparent index clear pixels take.
struct Plan {
    palette: Vec<Rgba8>,
    transparent: Option<u8>,
    masked: bool,
}

impl Plan {
    /// Read the picture once to find which entries its shown pixels use and
    /// whether any pixel is clear.
    fn of(
        source: &dyn PictureSource,
        rows: &mut RowBuffers,
        palette: &[Rgba8],
        masked: bool,
    ) -> Result<Self, EncodeError> {
        let mut shown = [false; 256];
        let mut hidden = false;
        for y in 0..source.height() {
            rows.read(source, y);
            indices_fit(&rows.samples, palette.len())?;
            for (x, &index) in rows.samples.iter().enumerate() {
                if visible(palette, &rows.mask, masked, x, index) {
                    shown[usize::from(index)] = true;
                } else {
                    hidden = true;
                }
            }
        }
        let mut written = fallible::collected(palette.len() + 1, palette.iter().copied())
            .ok_or(EncodeError::OutOfMemory)?;
        let transparent = if hidden {
            let clear_unshown =
                (0..palette.len()).find(|&at| palette[at][3] < SHOWN_FROM && !shown[at]);
            let slot = if let Some(at) = clear_unshown {
                at
            } else if written.len() < IndexDepth::Eight.colours() {
                written.push([0; 4]);
                written.len() - 1
            } else {
                (0..palette.len())
                    .find(|&at| !shown[at])
                    .ok_or(EncodeError::GifPaletteFull)?
            };
            Some(u8::try_from(slot).map_err(|_| EncodeError::InvalidPalette)?)
        } else {
            None
        };
        Ok(Self {
            palette: written,
            transparent,
            masked,
        })
    }

    /// The indices row `rows` is written as: a clear pixel takes the
    /// transparent index.
    fn write_row(&self, rows: &RowBuffers, out: &mut [u8]) {
        for (x, (slot, &index)) in out.iter_mut().zip(&rows.samples).enumerate() {
            *slot = match self.transparent {
                Some(transparent) if !visible(&self.palette, &rows.mask, self.masked, x, index) => {
                    transparent
                }
                _ => index,
            };
        }
    }
}

/// Whether pixel `x`, of entry `index`, shows: at least half opaque, its
/// entry's opacity seen through its mask.
fn visible(palette: &[Rgba8], mask: &[u8], masked: bool, x: usize, index: u8) -> bool {
    let entry = palette.get(usize::from(index)).copied().unwrap_or([0; 4]);
    let alpha = if masked {
        masked_colour(entry, mask.get(x).copied().unwrap_or(u8::MAX))[3]
    } else {
        entry[3]
    };
    alpha >= SHOWN_FROM
}

/// Codes packed least-significant bit first into data sub-blocks.
struct SubBlocks<'a> {
    out: &'a mut Output,
    block: [u8; SUB_BLOCK],
    filled: usize,
    bits: u32,
    held: u32,
}

impl<'a> SubBlocks<'a> {
    fn new(out: &'a mut Output) -> Self {
        Self {
            out,
            block: [0; SUB_BLOCK],
            filled: 0,
            bits: 0,
            held: 0,
        }
    }

    fn byte(&mut self, value: u8) -> Result<(), EncodeError> {
        self.block[self.filled] = value;
        self.filled += 1;
        if self.filled == SUB_BLOCK {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), EncodeError> {
        self.out
            .byte(u8::try_from(self.filled).map_err(|_| EncodeError::TooLarge)?)?;
        self.out.push(&self.block[..self.filled])?;
        self.filled = 0;
        Ok(())
    }

    /// Write the bits still held, the last sub-block, and the terminator.
    fn finish(mut self) -> Result<(), EncodeError> {
        if self.held > 0 {
            self.byte(self.bits.to_le_bytes()[0])?;
        }
        if self.filled > 0 {
            self.flush()?;
        }
        self.out.byte(0)
    }
}

impl CodeSink for SubBlocks<'_> {
    fn put(&mut self, code: u16, width: u32) -> Result<(), EncodeError> {
        self.bits |= u32::from(code) << self.held;
        self.held += width;
        while self.held >= 8 {
            self.byte(self.bits.to_le_bytes()[0])?;
            self.bits >>= 8;
            self.held -= 8;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "gif_encode_tests.rs"]
mod tests;
