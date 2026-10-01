//! Writing RISC OS sprite areas: every depth and mode word the decoder
//! reads, all three mask forms, and a sprite this crate cannot read written
//! back as the bytes it was read as.
//!
//! A sprite is written from its mode, and the encoder refuses a picture the
//! mode cannot state rather than quietly changing either: a partial mask
//! under a mode with only a binary one, transparency with no mask or alpha
//! channel to hold it, or a palette that is not the colours the pixels are
//! shown in. Choosing a mode that can hold a picture is the caller's
//! decision, made visibly, never the encoder's.

use crate::encode::{indices_fit, palette_fits, scratch, EncodeError, Output, RowBuffers};
use crate::picture::{IndexDepth, PictureKind, PictureSource, Rgba8};
use crate::sprite::{
    desktop_palette, mask_stride, opaque_sprite_writes_back, IndexedPalette, Layout, MaskDepth,
    SpriteMode, SpriteName, SpritePalette, AREA_HEADER_LEN, AREA_OFFSET_BIAS, PALETTE_ENTRY_LEN,
    SPRITE_HEADER_LEN,
};
use crate::RGBA_BYTES;

/// One sprite to write.
#[derive(Copy, Clone)]
pub enum SpriteInput<'a> {
    /// A sprite written from pixels.
    Picture {
        /// Its name.
        name: SpriteName,
        /// Its mode, which decides how the pixels are laid out.
        mode: SpriteMode,
        /// How a paletted sprite's colours are stated;
        /// [`SpritePalette::Implied`] for a direct colour one.
        palette: &'a SpritePalette,
        /// Whether to write a mask. A paletted picture's mask is its own
        /// alpha plane and must be present exactly when this is; a direct
        /// one's is its alpha.
        masked: bool,
        /// The pixels.
        source: &'a dyn PictureSource,
    },
    /// A sprite kept as the bytes it was read as, control block first, and
    /// written back exactly but for its length word, restated. It must be a
    /// whole number of words, as the sprites after it are laid on its end.
    Opaque(&'a [u8]),
}

pub(crate) fn encode(sprites: &[SpriteInput<'_>]) -> Result<alloc::vec::Vec<u8>, EncodeError> {
    if sprites.is_empty() {
        return Err(EncodeError::SpriteAreaEmpty);
    }
    let mut out = Output::new();
    out.le_u32(u32::try_from(sprites.len()).map_err(|_| EncodeError::TooLarge)?)?;
    out.le_u32(AREA_HEADER_LEN + AREA_OFFSET_BIAS)?;
    let end_at = out.len();
    out.le_u32(0)?;
    for sprite in sprites {
        match *sprite {
            SpriteInput::Picture {
                name,
                mode,
                palette,
                masked,
                source,
            } => write_sprite(&mut out, name, mode, palette, masked, source)?,
            SpriteInput::Opaque(bytes) => write_opaque(&mut out, bytes)?,
        }
    }
    let end = u32::try_from(out.len())
        .ok()
        .and_then(|len| len.checked_add(AREA_OFFSET_BIAS))
        .ok_or(EncodeError::TooLarge)?;
    out.patch_le_u32(end_at, end);
    Ok(out.into_bytes())
}

/// Write a kept sprite back exactly, its length word restated as the bytes
/// it heads.
///
/// Never padded, even to a whole word: a kept sprite may declare contents
/// running past its own end, and bytes appended to it would become part of
/// what it decodes to.
fn write_opaque(out: &mut Output, bytes: &[u8]) -> Result<(), EncodeError> {
    if !opaque_sprite_writes_back(bytes) {
        return Err(EncodeError::SpriteOpaqueMalformed);
    }
    out.le_u32(u32::try_from(bytes.len()).map_err(|_| EncodeError::TooLarge)?)?;
    out.push(&bytes[4..])
}

/// Bytes one row occupies at `bits` per pixel, word-aligned.
fn row_stride(width: u32, bits: u32) -> Result<u32, EncodeError> {
    mask_stride(width, bits).map_err(|_| EncodeError::TooLarge)
}

/// A palette entry pair as a control block holds it: `&BBGGRR00`, twice,
/// the second word being the flash colour.
fn palette_entry(out: &mut Output, entry: Rgba8) -> Result<(), EncodeError> {
    let word = u32::from(entry[0]) << 8 | u32::from(entry[1]) << 16 | u32::from(entry[2]) << 24;
    out.le_u32(word)?;
    out.le_u32(word)
}

/// Whether `palette` is exactly the opaque colours `expected` names.
fn same_colours(palette: &[Rgba8], expected: impl ExactSizeIterator<Item = [u8; 3]>) -> bool {
    palette.len() == expected.len()
        && palette
            .iter()
            .zip(expected)
            .all(|(entry, [r, g, b])| *entry == [r, g, b, u8::MAX])
}

/// The palette bytes a paletted sprite is written with, after checking they
/// state the colours its pixels are shown in.
fn check_palette(
    form: &SpritePalette,
    depth: IndexDepth,
    palette: &[Rgba8],
) -> Result<(), EncodeError> {
    match form {
        SpritePalette::Implied => {
            if !same_colours(palette, desktop_palette(depth).iter().copied()) {
                return Err(EncodeError::SpritePaletteMismatch);
            }
        }
        SpritePalette::Stored(raw) => {
            if !SpritePalette::stores(raw) {
                return Err(EncodeError::SpritePaletteMismatch);
            }
            let entries = raw.len() / PALETTE_ENTRY_LEN as usize;
            let resolved =
                IndexedPalette::new(depth.bits(), raw, u32::try_from(entries).unwrap_or(0));
            let colours = resolved.entries[..depth.colours()]
                .iter()
                .map(|entry| [entry[0], entry[1], entry[2]]);
            if !same_colours(palette, colours) {
                return Err(EncodeError::SpritePaletteMismatch);
            }
        }
        SpritePalette::Full => {
            if palette.iter().any(|entry| entry[3] != u8::MAX) {
                return Err(EncodeError::SpritePaletteAlpha);
            }
        }
    }
    Ok(())
}

/// How one sprite's rows are laid out on disk.
struct Shape {
    width: u32,
    height: u32,
    bits: u32,
    stride: u32,
    mask: Option<(MaskDepth, u32)>,
}

fn write_sprite(
    out: &mut Output,
    name: SpriteName,
    mode: SpriteMode,
    form: &SpritePalette,
    masked: bool,
    source: &dyn PictureSource,
) -> Result<(), EncodeError> {
    let parsed = mode.parsed();
    let (width, height) = (source.width(), source.height());
    if width == 0 || height == 0 {
        return Err(EncodeError::TooLarge);
    }
    let palette_len = match (parsed.layout, source.kind()) {
        (
            Layout::Indexed { bits },
            PictureKind::Indexed {
                depth,
                palette,
                masked: plane,
            },
        ) if depth.bits() == bits && plane == masked => {
            palette_fits(depth, palette)?;
            check_palette(form, depth, palette)?;
            match form {
                SpritePalette::Implied => 0,
                SpritePalette::Stored(raw) => raw.len(),
                SpritePalette::Full => depth.colours() * PALETTE_ENTRY_LEN as usize,
            }
        }
        (Layout::Packed { .. }, PictureKind::Rgba) if *form == SpritePalette::Implied => 0,
        _ => return Err(EncodeError::SpriteLayoutMismatch),
    };
    let bits = parsed.layout.bits();
    let stride = row_stride(width, bits)?;
    let mask = masked
        .then(|| {
            let mask_bits = match parsed.mask {
                MaskDepth::Image => bits,
                MaskDepth::Bit => 1,
                MaskDepth::Alpha => 8,
            };
            row_stride(width, mask_bits).map(|mask_stride| (parsed.mask, mask_stride))
        })
        .transpose()?;
    let shape = Shape {
        width,
        height,
        bits,
        stride,
        mask,
    };
    let image_bytes = u64::from(stride) * u64::from(height);
    let mask_bytes = mask.map_or(0, |(_, mask_stride)| {
        u64::from(mask_stride) * u64::from(height)
    });
    let image_at = u64::from(SPRITE_HEADER_LEN) + palette_len as u64;
    let to_u32 = |value: u64| u32::try_from(value).map_err(|_| EncodeError::TooLarge);
    let mask_at = image_at
        .checked_add(image_bytes)
        .ok_or(EncodeError::TooLarge)?;
    let length = to_u32(
        mask_at
            .checked_add(mask_bytes)
            .ok_or(EncodeError::TooLarge)?,
    )?;

    out.le_u32(length)?;
    let mut padded_name = [0u8; SpriteName::MAX_LEN];
    padded_name[..name.as_bytes().len()].copy_from_slice(name.as_bytes());
    out.push(&padded_name)?;
    out.le_u32(stride / 4 - 1)?;
    out.le_u32(height - 1)?;
    out.le_u32(0)?;
    out.le_u32(to_u32((u64::from(width) * u64::from(bits) - 1) % 32)?)?;
    out.le_u32(to_u32(image_at)?)?;
    out.le_u32(if mask.is_some() {
        to_u32(mask_at)?
    } else {
        to_u32(image_at)?
    })?;
    out.le_u32(mode.value())?;
    match (form, source.kind()) {
        (SpritePalette::Stored(raw), _) => out.push(raw)?,
        (SpritePalette::Full, PictureKind::Indexed { depth, palette, .. }) => {
            for index in 0..depth.colours() {
                palette_entry(out, palette.get(index).copied().unwrap_or([0, 0, 0, 255]))?;
            }
        }
        _ => {}
    }
    write_rows(out, source, &shape, parsed.layout)
}

/// Pack `value`, `bits` wide, at bit `bit` of `row`, least significant pixel
/// leftmost as a sprite lays them.
fn put_bits(row: &mut [u8], bit: u64, bits: u32, value: u32) {
    let mut bit = bit;
    let mut value = value;
    let mut left = bits;
    while left > 0 {
        let at = (bit / 8) as usize;
        let offset = u32::try_from(bit % 8).unwrap_or(0);
        let take = (8 - offset).min(left);
        if let Some(byte) = row.get_mut(at) {
            let part = value & ((1 << take) - 1);
            *byte |= u8::try_from(part << offset).unwrap_or(0);
        }
        value >>= take;
        bit += u64::from(take);
        left -= take;
    }
}

/// Write the image rows and then the mask rows.
fn write_rows(
    out: &mut Output,
    source: &dyn PictureSource,
    shape: &Shape,
    layout: Layout,
) -> Result<(), EncodeError> {
    let mut rows = RowBuffers::for_source(source)?;
    let mut line = scratch(shape.stride as usize)?;
    let alpha_channel = match layout {
        Layout::Packed { channels, .. } => channels[3].present(),
        Layout::Indexed { .. } => false,
    };
    let kind = source.kind();
    for y in 0..shape.height {
        rows.read(source, y);
        line.fill(0);
        match (layout, kind) {
            (Layout::Indexed { bits }, PictureKind::Indexed { palette, .. }) => {
                indices_fit(&rows.samples, palette.len())?;
                for (x, &index) in (0u64..).zip(&rows.samples) {
                    put_bits(&mut line, x * u64::from(bits), bits, u32::from(index));
                }
            }
            (Layout::Packed { bytes, channels }, PictureKind::Rgba) => {
                let bytes = bytes as usize;
                for (x, pixel) in rows.samples.as_chunks::<RGBA_BYTES>().0.iter().enumerate() {
                    if !alpha_channel && shape.mask.is_none() && pixel[3] != u8::MAX {
                        return Err(EncodeError::SpriteAlphaUnrepresentable);
                    }
                    let value = channels
                        .iter()
                        .zip(pixel)
                        .fold(0u32, |value, (channel, &sample)| {
                            value | channel.place(sample)
                        });
                    if let Some(slot) = line.get_mut(x * bytes..(x + 1) * bytes) {
                        slot.copy_from_slice(&value.to_le_bytes()[..bytes]);
                    }
                }
            }
            _ => return Err(EncodeError::SpriteLayoutMismatch),
        }
        out.push(&line)?;
    }
    let Some((depth, mask_stride)) = shape.mask else {
        return Ok(());
    };
    let mut mask_line = scratch(mask_stride as usize)?;
    for y in 0..shape.height {
        rows.read(source, y);
        mask_line.fill(0);
        for x in 0..shape.width as usize {
            let alpha = match kind {
                PictureKind::Indexed { .. } => rows.mask.get(x).copied().unwrap_or(0),
                PictureKind::Rgba => rows.samples.get(x * RGBA_BYTES + 3).copied().unwrap_or(0),
            };
            let at = x as u64;
            match depth {
                MaskDepth::Alpha => put_bits(&mut mask_line, at * 8, 8, u32::from(alpha)),
                MaskDepth::Image | MaskDepth::Bit => {
                    let bits = if depth == MaskDepth::Image {
                        shape.bits
                    } else {
                        1
                    };
                    let solid = match alpha {
                        0 => false,
                        u8::MAX => true,
                        _ => return Err(EncodeError::SpriteMaskNotBinary),
                    };
                    if solid {
                        put_bits(
                            &mut mask_line,
                            at * u64::from(bits),
                            bits,
                            u32::MAX >> (32 - bits),
                        );
                    }
                }
            }
        }
        out.push(&mask_line)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "sprite_encode_tests.rs"]
mod tests;
