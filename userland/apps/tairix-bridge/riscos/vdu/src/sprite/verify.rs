//! `OS_SpriteOp` 17, SpriteExtend's verification of an area, making the
//! checks RISC OS 5.30 was recorded making and no others: the free space
//! within the area; as many sprites as counted, each on a word from the first,
//! each ending by the free space; each sprite's wastage within a word, none at
//! the left of a new-format sprite; a RISC OS 5 mode word's fixed pattern;
//! its image within it, and its mask too where it has one. A numbered mode's
//! validity, resolutions, palette length and names are not checked.

use tairix_image::{sprite_area as block, SPRITE_HEADER_LEN};
use tairix_riscos_kernel::os::{Fail, Os, Regs};

use super::area::{Area, Naming};
use super::format::{self, Pixels};
use super::header::Sprite;
use super::UNRECOGNISED;

/// `OS_SpriteOp` 17: the area verified.
pub(crate) fn verify(os: &mut Os<'_>, naming: Naming, regs: &Regs) -> Result<(), Fail> {
    let area = Area::named(os, naming, regs.r[1])?;
    if sound(os, area)? {
        Ok(())
    } else {
        Err(os.error(UNRECOGNISED))
    }
}

/// Whether the area passes.
fn sound(os: &mut Os<'_>, area: Area) -> Result<bool, Fail> {
    let block = area.block(os)?;
    if block.free > block.size || block.first < block::LEN || !block.first.is_multiple_of(4) {
        return Ok(false);
    }
    let mut offset = block.first;
    for counted in 0..block.count {
        let header_end = u64::from(offset) + u64::from(SPRITE_HEADER_LEN);
        if !offset.is_multiple_of(4) || header_end > u64::from(block.free) {
            return Ok(false);
        }
        let sprite = Sprite::read(os, area.address(offset))?;
        if u64::from(offset) + u64::from(sprite.next) > u64::from(block.free)
            || !sprite_sound(&sprite)
        {
            return Ok(false);
        }
        // A sprite claiming no length is the same sprite at every step after.
        if sprite.next == 0 {
            break;
        }
        offset = offset.wrapping_add(sprite.next);
        if counted % 256 == 255 {
            os.yield_point()?;
        }
    }
    Ok(true)
}

/// Whether one sprite passes. The image test is RISC OS 5.30's own, its
/// sum taken round the word: an image starting past the sprite's end
/// passes it.
fn sprite_sound(sprite: &Sprite) -> bool {
    if !format::verifiable(sprite.mode) {
        return false;
    }
    let pixels = Pixels::verified(sprite.mode);
    if !sprite.bits_sound(&pixels) {
        return false;
    }
    let image_room = sprite.next.wrapping_sub(sprite.image);
    if sprite
        .image_bytes()
        .is_none_or(|bytes| u64::from(image_room) < bytes)
    {
        return false;
    }
    if sprite.masked() {
        let mask_end = sprite
            .mask_bytes(&pixels)
            .and_then(|bytes| u64::from(sprite.mask).checked_add(bytes));
        if sprite.image.min(sprite.mask) < SPRITE_HEADER_LEN
            || mask_end.is_none_or(|end| end > u64::from(sprite.next))
        {
            return false;
        }
    }
    true
}
