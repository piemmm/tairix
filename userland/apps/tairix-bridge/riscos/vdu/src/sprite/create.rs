//! Sprite areas and the sprites in them as wholes: reading and initialising
//! an area, naming its sprites, making a blank sprite, and selecting,
//! deleting, renaming and copying one (`OS_SpriteOp` 8, 9, 13, 15 and
//! 24 to 27).

use alloc::vec::Vec;

use tairix_image::{sprite_area as block, SpriteName, SPRITE_HEADER_LEN, SPRITE_PALETTE_ENTRY_LEN};
use tairix_riscos_kernel::os::{len32, Entry, Fail, Os, Regs};

use super::area::{read_name, Area, Naming};
use super::format::{self, Pixels};
use super::header::Sprite;
use super::{
    system, NOT_FOUND, NO_DEEP_NEW_PALETTE, NO_MEMORY, NO_ROOM, NO_ROOM_TO_COPY, UNRECOGNISED,
};
use crate::palette::{reason as palette_reason, PALETTE_V};

/// An operation's sprite: the area it is in, how R0 named it, and its
/// control block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) area: Area,
    pub(crate) naming: Naming,
    pub(crate) sprite: Sprite,
}

/// `OS_SpriteOp` 8: R2 to R5 the area's size, count, first sprite and free
/// space, as its block holds them.
pub(crate) fn read_area(os: &mut Os<'_>, naming: Naming, regs: &mut Regs) -> Result<(), Fail> {
    let block = Area::named(os, naming, regs.r[1])?.block(os)?;
    regs.r[2] = block.size;
    regs.r[3] = block.count;
    regs.r[4] = block.first;
    regs.r[5] = block.free;
    Ok(())
}

/// `OS_SpriteOp` 9: no sprites, the free space where the first would be;
/// the system area forgets its selection.
pub(crate) fn initialise(os: &mut Os<'_>, naming: Naming, regs: &mut Regs) -> Result<(), Fail> {
    let area = Area::named(os, naming, regs.r[1])?;
    let first = area.block(os)?.first;
    area.set(os, block::COUNT, 0)?;
    area.set(os, block::FREE, first)?;
    if area.system {
        system::select(os, None);
    }
    Ok(())
}

/// `OS_SpriteOp` 13: sprite R4's name, numbered from one, into R2's buffer
/// of R3 bytes, ending with a zero within it; R3 its length.
pub(crate) fn name(os: &mut Os<'_>, naming: Naming, regs: &mut Regs) -> Result<(), Fail> {
    let area = Area::named(os, naming, regs.r[1])?;
    let Some(sprite) = area.nth(os, regs.r[4])? else {
        return Err(os.error(NOT_FOUND));
    };
    let Some(room) = regs.r[3].checked_sub(1) else {
        regs.r[3] = 0;
        return Ok(());
    };
    let name = sprite.name.as_bytes();
    let length = len32(name.len()).min(room);
    os.write_bytes(regs.r[2], &name[..length as usize])?;
    os.write_byte(regs.r[2].wrapping_add(length), 0)?;
    regs.r[3] = length;
    Ok(())
}

/// `OS_SpriteOp` 15: a blank sprite named R2, R4 by R5 pixels in mode R6,
/// with a palette copied from the screen's where R3 asks; one already of
/// that name is deleted first.
pub(crate) fn create(os: &mut Os<'_>, naming: Naming, regs: &mut Regs) -> Result<(), Fail> {
    let area = Area::named(os, naming, regs.r[1])?;
    let name = read_name(os, regs.r[2])?.lowered();
    let (pixels, word) = format::for_new_sprite(os, regs.r[6])?;
    let entries = if regs.r[3] == 0 {
        0
    } else if pixels.paletted() {
        new_palette_entries(&pixels)
    } else {
        return Err(os.error(NO_DEEP_NEW_PALETTE));
    };
    let shape = Shape::new(&pixels, regs.r[4], regs.r[5], 0);
    let palette = entries * SPRITE_PALETTE_ENTRY_LEN;
    let length = shape.length(palette);
    let screen = screen_palette(os, entries)?;
    if let Some(old) = area.find(os, &name)? {
        area.remove(os, &old)?;
    }
    let Some(length) = length else {
        return Err(os.error(NO_ROOM));
    };
    let at = area.append(os, length, NO_ROOM)?;
    let sprite = Sprite {
        at: area.address(at),
        next: length,
        name,
        words_less_one: shape.words.wrapping_sub(1),
        rows_less_one: regs.r[5].wrapping_sub(1),
        first_bit: 0,
        last_bit: shape.last_bit,
        image: SPRITE_HEADER_LEN + palette,
        mask: SPRITE_HEADER_LEN + palette,
        mode: word,
    };
    sprite.write(os)?;
    os.write_bytes(sprite.address(SPRITE_HEADER_LEN), &screen)?;
    os.fill(
        sprite.address(sprite.image),
        length - SPRITE_HEADER_LEN - palette,
        0,
    )
}

/// The entries a palette a new sprite is given holds: one a colour, an
/// eight-bit sprite's being VIDC1's sixty-four.
pub(crate) const fn new_palette_entries(pixels: &Pixels) -> u32 {
    match pixels.log2_bpp {
        0 => 2,
        1 => 4,
        2 => 16,
        _ => 64,
    }
}

/// The screen's first `entries` palette entries as `OS_ReadPalette` gives
/// them, each state's word in turn: read before the sprite they are for is
/// made, so that no claimant of `PaletteV` meets it half made.
pub(crate) fn screen_palette(os: &mut Os<'_>, entries: u32) -> Result<Vec<u8>, Fail> {
    let mut bytes = Vec::new();
    if bytes
        .try_reserve_exact(entries as usize * SPRITE_PALETTE_ENTRY_LEN as usize)
        .is_err()
    {
        return Err(os.error(NO_MEMORY));
    }
    for entry in 0..entries {
        let mut read = Regs::new([
            entry,
            16,
            0,
            0,
            palette_reason::READ,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
        os.call_vector(PALETTE_V, &mut read, Entry::Supervisor)?;
        bytes.extend_from_slice(&read.r[2].to_le_bytes());
        bytes.extend_from_slice(&read.r[3].to_le_bytes());
    }
    Ok(bytes)
}

/// A new sprite's rows: the words each takes and its last bit, its first
/// pixel at bit `first_bit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Shape {
    pub(crate) words: u32,
    pub(crate) last_bit: u32,
    /// The bytes its image takes; none where they would not fit a word.
    pub(crate) image: Option<u32>,
    /// The bytes a mask for it takes; none likewise.
    pub(crate) mask: Option<u32>,
    /// The bytes each row of a mask for it takes: an old-format sprite's its
    /// image's, a new-format one's none past what a word counts.
    pub(crate) mask_stride: Option<u64>,
}

impl Shape {
    /// The rows of `width` by `height` pixels of `pixels`, from bit
    /// `first_bit` of each: a width of nought is -1 words.
    pub(crate) fn new(pixels: &Pixels, width: u32, height: u32, first_bit: u32) -> Self {
        let bits = u64::from(first_bit) + (u64::from(width) << pixels.log2_field);
        let words = bits.div_ceil(32);
        let last_bit = u32::try_from(bits.wrapping_sub(1) % 32).unwrap_or(31);
        let image = words.checked_mul(4 * u64::from(height));
        let mask_stride = if pixels.old_format {
            Some(words * 4)
        } else {
            tairix_image::sprite_mask_stride(width, pixels.mask_bits())
                .ok()
                .map(u64::from)
        };
        let mask = mask_stride.and_then(|stride| stride.checked_mul(u64::from(height)));
        let fit = |bytes: Option<u64>| bytes.and_then(|b| u32::try_from(b).ok());
        Self {
            words: u32::try_from(words).unwrap_or(u32::MAX),
            last_bit,
            image: fit(image),
            mask: fit(mask),
            mask_stride,
        }
    }

    /// A sprite's whole length with a palette of `palette` bytes and no mask;
    /// none where it would not fit a word.
    pub(crate) fn length(&self, palette: u32) -> Option<u32> {
        self.image?
            .checked_add(SPRITE_HEADER_LEN)?
            .checked_add(palette)
    }
}

/// `OS_SpriteOp` 24: the system area's sprite selected, R2 left as given; a
/// user area's sprite's address in R2.
pub(crate) fn select(os: &mut Os<'_>, target: &Target, regs: &mut Regs) {
    if target.area.system {
        system::select(os, Some(target.sprite.name));
    } else {
        regs.r[2] = target.sprite.at;
    }
}

/// `OS_SpriteOp` 25: the sprite deleted, the area closed up after it.
pub(crate) fn delete(os: &mut Os<'_>, target: &Target) -> Result<(), Fail> {
    target.area.remove(os, &target.sprite)
}

/// `OS_SpriteOp` 26: the sprite renamed R3's name, in lower case; nothing
/// at all where a sprite of that name is already in the area, the sprite
/// itself included.
pub(crate) fn rename(os: &mut Os<'_>, target: &Target, regs: &mut Regs) -> Result<(), Fail> {
    let name = read_name(os, regs.r[3])?.lowered();
    if target.area.find(os, &name)?.is_some() {
        return Ok(());
    }
    Sprite {
        name,
        ..target.sprite
    }
    .write(os)
}

/// `OS_SpriteOp` 27: a copy of the sprite named R3's name appended to the
/// area; nothing at all where a sprite of that name is already in it.
pub(crate) fn copy(os: &mut Os<'_>, target: &Target, regs: &mut Regs) -> Result<(), Fail> {
    let name = read_name(os, regs.r[3])?.lowered();
    if target.area.find(os, &name)?.is_some() {
        return Ok(());
    }
    let length = target.sprite.next;
    let from = target.sprite.at;
    let readable = os.readable(from, u64::from(from) + u64::from(length));
    if length < SPRITE_HEADER_LEN || !length.is_multiple_of(4) || !readable {
        return Err(os.error(UNRECOGNISED));
    }
    let at = target.area.append(os, length, NO_ROOM_TO_COPY)?;
    let copied = target.area.address(at);
    os.copy_bytes(target.sprite.at, copied, length)?;
    Sprite {
        at: copied,
        name,
        ..target.sprite
    }
    .write(os)
}

/// The name a sprite of the system area made by `VDU 23,27` and its kin
/// takes: number `n` spelt in decimal.
pub(crate) fn numbered_name(n: u8) -> SpriteName {
    let mut digits = [0u8; 12];
    SpriteName::spelt_by(tairix_util::fmt::format_i32(i32::from(n), &mut digits).as_bytes())
}
