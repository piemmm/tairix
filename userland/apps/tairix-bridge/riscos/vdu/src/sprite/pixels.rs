//! A sprite's size and its pixels one at a time (`OS_SpriteOp` 40 to 44).
//! Pixels are numbered from the bottom left. A colour is masked to the
//! sprite's depth and repeated across a doubling mode's wider field; an
//! eight-bit sprite with fewer than 256 palette entries takes and gives its
//! pixels as a six-bit colour number and a tint, as `GCOL` does.

use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_util::bitfield::{field_at, mask, set_field_at};
use tairix_vdu::mode::repeated;

use super::create::Target;
use super::format::Pixels;
use super::header::{advanced, Sprite};
use super::{OUTSIDE, UNRECOGNISED};
use crate::colour::{internal, number_of_internal};

/// `OS_SpriteOp` 40: R3 and R4 its pixels across and down, R5 one where it
/// has a mask, R6 its mode.
pub(crate) fn info(os: &mut Os<'_>, target: &Target, regs: &mut Regs) -> Result<(), Fail> {
    let sprite = &target.sprite;
    let pixels = sprite.pixels(os)?;
    regs.r[3] = sprite.width(&pixels);
    regs.r[4] = sprite.rows();
    regs.r[5] = u32::from(sprite.masked());
    regs.r[6] = sprite.mode;
    Ok(())
}

/// Where pixel (R3, R4) lies: its image row's offset from the sprite's start
/// and its bit along the row, and the pixels; "Outside the sprite" for a
/// pixel it does not have.
fn place(os: &mut Os<'_>, sprite: &Sprite, regs: &Regs) -> Result<(Pixels, u32, u32), Fail> {
    let pixels = sprite.pixels_kept(os)?;
    let (x, y) = (regs.r[3], regs.r[4]);
    if x >= sprite.width(&pixels) || y >= sprite.rows() {
        return Err(os.error(OUTSIDE));
    }
    Ok((pixels, x, sprite.rows() - 1 - y))
}

/// The address of row `row` of the block at the sprite's byte `start`, its
/// rows `stride` bytes long: summed in a word, as RISC OS 5 sums it, since a
/// pixel's SWIs take the control block as it stands.
fn row_address(sprite: &Sprite, start: u32, row: u32, stride: u64) -> u32 {
    let stride = u32::try_from(stride & u64::from(u32::MAX)).unwrap_or(0);
    sprite.address(start).wrapping_add(row.wrapping_mul(stride))
}

/// Where pixel `x` of image row `row` lies: the address of the row's first
/// byte and the bit along it.
fn image_place(sprite: &Sprite, pixels: &Pixels, x: u32, row: u32) -> (u32, u64) {
    let address = row_address(sprite, sprite.image, row, sprite.stride());
    let bit = u64::from(sprite.first_bit) + (u64::from(x) << pixels.log2_field);
    (address, bit)
}

/// The `bits` at bit `bit` from `address`.
fn read_field(os: &mut Os<'_>, address: u32, bit: u64, bits: u32) -> Result<u32, Fail> {
    let (start, shift, length) = span(address, bit, bits);
    let mut bytes = [0u8; 8];
    os.read_bytes(start, &mut bytes[..length])?;
    Ok(field_at(&bytes, u64::from(shift), bits))
}

/// Writes `value` as the `bits` at bit `bit` from `address`, the bits around
/// them kept.
fn write_field(os: &mut Os<'_>, address: u32, bit: u64, bits: u32, value: u32) -> Result<(), Fail> {
    let (start, shift, length) = span(address, bit, bits);
    let mut bytes = [0u8; 8];
    os.read_bytes(start, &mut bytes[..length])?;
    set_field_at(&mut bytes, u64::from(shift), bits, value);
    os.write_bytes(start, &bytes[..length])
}

/// The bytes holding `bits` at bit `bit` from `address`: their first address,
/// the field's shift within them, and how many there are.
fn span(address: u32, bit: u64, bits: u32) -> (u32, u32, usize) {
    let shift = u32::try_from(bit % 8).unwrap_or(0);
    (
        advanced(address, bit / 8),
        shift,
        (shift + bits).div_ceil(8) as usize,
    )
}

/// The colours a pixel of `bits` bits can hold.
fn colours_of(bits: u32) -> u32 {
    u32::try_from(mask(bits)).unwrap_or(u32::MAX)
}

/// `OS_SpriteOp` 41: pixel (R3, R4)'s colour in R5 and tint in R6.
pub(crate) fn read_pixel(os: &mut Os<'_>, target: &Target, regs: &mut Regs) -> Result<(), Fail> {
    let sprite = &target.sprite;
    let (pixels, x, row) = place(os, sprite, regs)?;
    let (address, bit) = image_place(sprite, &pixels, x, row);
    let value = read_field(os, address, bit, pixels.bits())?;
    (regs.r[5], regs.r[6]) = if tinted(sprite, &pixels) {
        number_of_internal(value.to_le_bytes()[0])
    } else {
        (value, 0)
    };
    Ok(())
}

/// `OS_SpriteOp` 42: pixel (R3, R4) set to colour R5 with tint R6.
pub(crate) fn write_pixel(os: &mut Os<'_>, target: &Target, regs: &mut Regs) -> Result<(), Fail> {
    let sprite = &target.sprite;
    let (pixels, x, row) = place(os, sprite, regs)?;
    let value = if tinted(sprite, &pixels) {
        u32::from(internal(regs.r[5], regs.r[6]))
    } else {
        regs.r[5] & colours_of(pixels.bits())
    };
    let field = pixels.field_bits();
    let (address, bit) = image_place(sprite, &pixels, x, row);
    write_field(
        os,
        address,
        bit,
        field,
        repeated(value, pixels.bits(), field),
    )
}

/// Whether the sprite's pixels are colour numbers and tints: eight bits a
/// colour, through no palette of every one.
fn tinted(sprite: &Sprite, pixels: &Pixels) -> bool {
    pixels.log2_bpp == 3 && !sprite.full_palette()
}

/// Where pixel `x` of mask row `row` lies: the row's address and the bit
/// along it, and the bits a mask pixel takes; "Unrecognised sprite data"
/// where a mask row's length passes what a word counts.
fn mask_place(
    os: &mut Os<'_>,
    sprite: &Sprite,
    pixels: &Pixels,
    x: u32,
    row: u32,
) -> Result<(u32, u64, u32), Fail> {
    let stride = sprite
        .mask_stride(pixels)
        .ok_or_else(|| os.error(UNRECOGNISED))?;
    let address = row_address(sprite, sprite.mask, row, stride);
    let bits = pixels.mask_bits();
    let bit = if pixels.old_format {
        u64::from(sprite.first_bit) + (u64::from(x) << pixels.log2_field)
    } else {
        u64::from(x) * u64::from(bits)
    };
    Ok((address, bit, bits))
}

/// `OS_SpriteOp` 43: R5 the mask at pixel (R3, R4), one or nought, or an
/// alpha mask's value; a sprite with no mask is opaque throughout.
pub(crate) fn read_mask(os: &mut Os<'_>, target: &Target, regs: &mut Regs) -> Result<(), Fail> {
    let sprite = &target.sprite;
    let (pixels, x, row) = place(os, sprite, regs)?;
    let opaque = if pixels.wide_mask && !pixels.old_format {
        0xFF
    } else {
        1
    };
    regs.r[5] = if sprite.masked() {
        let (address, bit, bits) = mask_place(os, sprite, &pixels, x, row)?;
        let value = read_field(os, address, bit, bits)?;
        if bits == 8 && !pixels.old_format {
            value
        } else {
            u32::from(value != 0)
        }
    } else {
        opaque
    };
    Ok(())
}

/// `OS_SpriteOp` 44: the mask at pixel (R3, R4) set from R5, an alpha mask's
/// value or else opaque where it is not nought; nothing for a sprite with
/// no mask.
pub(crate) fn write_mask(os: &mut Os<'_>, target: &Target, regs: &mut Regs) -> Result<(), Fail> {
    let sprite = &target.sprite;
    let (pixels, x, row) = place(os, sprite, regs)?;
    if !sprite.masked() {
        return Ok(());
    }
    let (address, bit, bits) = mask_place(os, sprite, &pixels, x, row)?;
    let value = if bits == 8 && !pixels.old_format {
        regs.r[5] & 0xFF
    } else if regs.r[5] != 0 {
        colours_of(bits)
    } else {
        0
    };
    write_field(os, address, bit, bits, value)
}
