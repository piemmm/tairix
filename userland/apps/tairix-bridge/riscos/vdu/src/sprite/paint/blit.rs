//! The kernel's plots, `OS_SpriteOp` 28, 34, 48 and 49: a sprite's rows laid
//! on the destination bit for bit from the pixel at its bottom left, however
//! the two depths differ, so a pixel of the sprite covers as many of the
//! destination's as its bits fill. 28 and 34 combine each bit with the
//! destination's by the plot action, where asked only under a solid mask;
//! 48 and 49 plot the background colour under it, or over a rectangle of
//! the sprite's width and height in pixels where it has no mask. A mask
//! pixel covers its image pixel's bits. An eight-bit alpha mask is
//! SpriteExtend's alone: the kernel's plots refuse one where they would use
//! it.

use alloc::vec::Vec;

use tairix_riscos_kernel::os::{Fail, Os};
use tairix_riscos_kernel::output::workspace;

use super::super::edit::{field_at, set_field_at};
use super::super::format::BAD_SPRITE_MODE;
use super::super::header::Sprite;
use super::super::NO_MEMORY;
use super::source::{Source, Transparency};
use super::{changed, read, Destination};
use crate::canvas::{mask_between, Block};
use crate::raster::Point;

/// What a blit lays down.
#[derive(Clone, Copy)]
enum Ink {
    /// The sprite's bits, by `action`, under its mask where `masked`.
    Sprite { action: u32, masked: bool },
    /// A colour block, under the mask.
    Colour(Block),
}

/// `OS_SpriteOp` 28 and 34: the sprite at `at`, R5's action in its low three
/// bits and the mask asked for by bit 3; an alpha channel is copied with the
/// colours, the mask asked for or not.
pub(crate) fn put(os: &mut Os<'_>, sprite: &Sprite, at: Point, plot: u32) -> Result<(), Fail> {
    let source = read(os, sprite)?;
    let masked = plot & 8 != 0;
    if masked && source.transparency == Transparency::Wide {
        return Err(os.error(BAD_SPRITE_MODE));
    }
    let masked = masked && source.transparency == Transparency::Binary;
    blit(
        os,
        &source,
        at,
        Ink::Sprite {
            action: plot & 7,
            masked,
        },
    )
}

/// `OS_SpriteOp` 48 and 49: the background colour plotted at `at` under the
/// sprite's mask, or as a solid rectangle of its size without one.
pub(crate) fn plot_mask(os: &mut Os<'_>, sprite: &Sprite, at: Point) -> Result<(), Fail> {
    let block = Block::read(os, workspace::BACKGROUND)?;
    plot_shape(os, sprite, at, block)
}

/// Colour block `block` plotted at `at` under the sprite's mask, or as a
/// solid rectangle of its size without one.
pub(crate) fn plot_shape(
    os: &mut Os<'_>,
    sprite: &Sprite,
    at: Point,
    block: Block,
) -> Result<(), Fail> {
    let source = read(os, sprite)?;
    match source.transparency {
        Transparency::Binary => return blit(os, &source, at, Ink::Colour(block)),
        Transparency::Wide => return Err(os.error(BAD_SPRITE_MODE)),
        Transparency::None | Transparency::Channel(_) => {}
    }
    let destination = Destination::now(os)?;
    let Some(rect) = destination.visible(at, source.width, source.height) else {
        return Ok(());
    };
    let canvas = destination.canvas;
    for y in rect.bottom..=rect.top {
        let row = canvas.row_of(y);
        let (ora, eor) = block.for_line(row);
        canvas.span(
            os,
            row,
            rect.left.cast_unsigned(),
            rect.right.cast_unsigned(),
            ora,
            eor,
        )?;
    }
    changed(os, rect)
}

/// Lays `source`'s rows on the destination from `at`.
fn blit(os: &mut Os<'_>, source: &Source, at: Point, ink: Ink) -> Result<(), Fail> {
    let destination = Destination::now(os)?;
    let log2_bpc = destination.format.log2_bpc();
    let covered = u32::try_from(source.row_bits.div_ceil(1 << log2_bpc)).unwrap_or(u32::MAX);
    let Some(visible) = destination.visible(at, covered, source.height) else {
        return Ok(());
    };
    let origin = i64::from(at.0) << log2_bpc;
    let low = (i64::from(visible.left) << log2_bpc).max(origin);
    let high = ((i64::from(visible.right) + 1) << log2_bpc)
        .min(origin + i64::try_from(source.row_bits).unwrap_or(i64::MAX));
    if low >= high {
        return Ok(());
    }
    let first_byte = u32::try_from(low / 8).unwrap_or(0);
    let length = usize::try_from((high + 7) / 8 - low / 8).unwrap_or(0);
    let mut line = Vec::new();
    if line.try_reserve_exact(length).is_err() {
        return Err(os.error(NO_MEMORY));
    }
    line.resize(length, 0);
    let canvas = destination.canvas;
    let base_bit = u64::try_from(low - i64::from(first_byte) * 8).unwrap_or(0);
    let span = u64::try_from(high - low).unwrap_or(0);
    let source_start = u64::from(source.first_bit) + u64::try_from(low - origin).unwrap_or(0);
    for y in visible.bottom..=visible.top {
        let row = source.height - 1 - (y - at.1).cast_unsigned();
        let screen_row = canvas.row_of(y);
        os.read_bytes(canvas.address(screen_row, first_byte), &mut line)?;
        let block = match ink {
            Ink::Colour(block) => block.for_line(screen_row),
            Ink::Sprite { .. } => (0, 0),
        };
        let mut done = 0u64;
        while done < span {
            let bits = u32::try_from((span - done).min(24)).unwrap_or(24);
            let at_line = base_bit + done;
            let at_source = source_start + done;
            let old = field_at(&line, at_line, bits);
            let covered_bits = match ink {
                Ink::Sprite { masked: false, .. } => mask_between(0, bits),
                Ink::Sprite { masked: true, .. } | Ink::Colour(_) => {
                    source.solid_bits(at_source, bits, row)
                }
            };
            let new = match ink {
                Ink::Sprite { action, .. } => combine(
                    action,
                    old,
                    field_at(source.image_row(row), at_source, bits),
                ),
                Ink::Colour(_) => {
                    let (ora, eor) = block;
                    let rotation =
                        u32::try_from((u64::from(first_byte) * 8 + at_line) % 32).unwrap_or(0);
                    let (ora, eor) = (ora.rotate_right(rotation), eor.rotate_right(rotation));
                    (old | ora) ^ eor
                }
            };
            set_field_at(
                &mut line,
                at_line,
                bits,
                (old & !covered_bits) | (new & covered_bits),
            );
            done += u64::from(bits);
        }
        os.write_bytes(canvas.address(screen_row, first_byte), &line)?;
    }
    changed(os, visible)
}

/// `new` from destination bits `old` and source bits `bits` by GCOL action
/// `action`.
pub(crate) const fn combine(action: u32, old: u32, bits: u32) -> u32 {
    match action & 7 {
        0 => bits,
        1 => old | bits,
        2 => old & bits,
        3 => old ^ bits,
        4 => !old,
        5 => old,
        6 => old & !bits,
        _ => old | !bits,
    }
}
