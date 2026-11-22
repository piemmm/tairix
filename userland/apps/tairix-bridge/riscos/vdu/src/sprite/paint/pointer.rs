//! `OS_SpriteOp` 36: a pointer shape made from a sprite of 256 colours or
//! fewer, its pixels turned into the pointer's four colours by a translation
//! table, or without one by their low two bits, scaled by the factors given
//! or by the eigen factors of the sprite and the screen, and cut to the
//! pointer's 32 by 32 from the top left. A masked pixel is the pointer's
//! clear colour. The sprite's palette gives the pointer's three colours: its
//! entries 1 to 3, or with a table the entries the table turns into them.

use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_vdu::mode::var;

use super::super::format::variable;
use super::super::header::Sprite;
use super::super::NO_DEEP_PALETTE;
use super::source::Transparency;
use super::{read, Scale};
use crate::palette::{self, kind};
use crate::pointer::Shape;
use crate::state::{self, with};

/// R3's bits: the shape, then what to leave alone.
const SHAPE_NUMBER: u32 = 0xF;
const KEEP_IMAGE: u32 = 0x10;
const KEEP_PALETTE: u32 = 0x20;
const KEEP_SELECTION: u32 = 0x40;
/// The pointer's pixels across and down at most.
const MOST_SIDE: u32 = 32;
/// The pointer's pixels to a byte.
const PIXELS_PER_BYTE: u32 = 4;
/// The pointer's colours past its clear one.
const POINTER_COLOURS: u32 = 3;

/// `OS_SpriteOp` 36: pointer shape R3 made from the sprite, its active point
/// R4 and R5 in pixels from its top left, R6's scale factors and R7's table.
pub(crate) fn set_pointer(os: &mut Os<'_>, sprite: &Sprite, regs: &Regs) -> Result<(), Fail> {
    let [_, _, _, flags, active_x, active_y, scale, table, ..] = regs.r;
    let number = flags & SHAPE_NUMBER;
    let source = read(os, sprite)?;
    if !source.pixels.paletted() {
        return Err(os.error(NO_DEEP_PALETTE));
    }
    let entries = 1usize << source.pixels.bits();
    let table = if table == 0 {
        None
    } else {
        Some(os.read_vec(table, entries)?)
    };
    let colour = |pixel: u32| match &table {
        Some(table) => table.get(pixel as usize).map_or(0, |&b| u32::from(b) & 3),
        None => pixel & 3,
    };
    if flags & KEEP_IMAGE == 0 && (1..=4).contains(&number) {
        let scale = if scale == 0 {
            default_scale(os, sprite)?
        } else {
            match Scale::read(os, scale)? {
                Some(scale) => scale,
                None => return Ok(()),
            }
        };
        let masked = source.transparency != Transparency::None && source.first_bit == 0;
        let pixel = |column: u32, row: u32| {
            (source.alpha(column, row) != 0).then(|| colour(source.colour(column, row)))
        };
        let size = scale.size(source.width, source.height);
        let shape = image(size, scale, pixel, masked, (active_x, active_y));
        with(os, |vdu, _| {
            vdu.pointer.shapes[(number - 1) as usize] = shape;
            Ok(())
        })?;
    }
    if flags & KEEP_PALETTE == 0 {
        for logical in 1..=POINTER_COLOURS {
            let index = match &table {
                Some(table) => table.iter().position(|&b| u32::from(b) == logical),
                None => Some(logical as usize),
            };
            let Some(&word) = index.and_then(|i| source.palette.get(i)) else {
                continue;
            };
            let [_, r, g, b] = word.to_le_bytes();
            let (logical, physical) = (logical.to_le_bytes()[0], kind::POINTER.to_le_bytes()[0]);
            palette::write_through(os, logical, physical, [r, g, b])?;
        }
    }
    if flags & KEEP_SELECTION == 0 && (1..=4).contains(&number) {
        state::with(os, |vdu, _| {
            vdu.pointer.selected = number.to_le_bytes()[0];
            Ok(())
        })?;
    }
    Ok(())
}

/// The factors the eigen factors of the sprite and the screen give: a pixel
/// of the sprite as many of the screen's as its OS units cover.
fn default_scale(os: &mut Os<'_>, sprite: &Sprite) -> Result<Scale, Fail> {
    let (screen_x, screen_y) = state::with(os, |vdu, _| {
        Ok((vdu.format().x_eig(), vdu.format().y_eig()))
    })?;
    let eig = |v| variable(sprite.mode, v).unwrap_or(1);
    let factor =
        |sprite_eig: u32, screen_eig: u32| (1u32 << sprite_eig.min(3), 1u32 << screen_eig.min(3));
    let (mul_x, div_x) = factor(eig(var::X_EIG), screen_x);
    let (mul_y, div_y) = factor(eig(var::Y_EIG), screen_y);
    Ok(Scale::of((mul_x, mul_y), (div_x, div_y)))
}

/// The pointer image of `size` pixels scaled by `scale`, cut to 32 by 32,
/// each pixel the pointer colour `pixel` gives its source pixel (a column
/// and a row from the top) or clear for none; the active point the first
/// solid pixel down from the top where `auto_active`, else `given`.
fn image(
    size: (u32, u32),
    scale: Scale,
    pixel: impl Fn(u32, u32) -> Option<u32>,
    auto_active: bool,
    given: (u32, u32),
) -> Shape {
    let (width, height) = (size.0.min(MOST_SIDE), size.1.min(MOST_SIDE));
    let bytes = width.div_ceil(PIXELS_PER_BYTE);
    let mut data = alloc::vec![0u8; (bytes * height) as usize];
    let mut active = None;
    for j in 0..height {
        let row = scale.row(j);
        for i in 0..width {
            let Some(colour) = pixel(scale.column(i), row) else {
                continue;
            };
            if auto_active && active.is_none() {
                active = Some((i, j));
            }
            let at = (j * bytes + i / PIXELS_PER_BYTE) as usize;
            if let Some(byte) = data.get_mut(at) {
                *byte |= ((colour & 3) << (2 * (i % PIXELS_PER_BYTE))).to_le_bytes()[0];
            }
        }
    }
    let (ax, ay) = active.unwrap_or(given);
    let byte = |v: u32| u8::try_from(v).unwrap_or(u8::MAX);
    Shape {
        width: byte(bytes),
        height: byte(height),
        active: (byte(ax), byte(ay)),
        data,
    }
}

#[cfg(test)]
mod tests;
