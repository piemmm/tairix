//! Joining two sprites (`OS_SpriteOp` 35): the second beside the first, on
//! its right, or below it. Both are removed and a new sprite holding both —
//! the first's name, mode and palette, its pixels from bit nought — is
//! appended to the area, as RISC OS 5 leaves it. A mask either has covers the
//! whole, every bit set but where a sprite's own mask clears one, as a new
//! mask's are. Nothing changes unless the area holds the result.

use tairix_image::SPRITE_HEADER_LEN;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_util::fallible;

use super::create::{Shape, Target};
use super::edit::row_buffer;
use super::format::Pixels;
use super::header::{advanced, Extents, Sprite};
use super::{CANNOT_JOIN, NO_MEMORY};
use tairix_util::bitfield::{field_at, set_field_at};

/// `OS_SpriteOp` 35: R3's sprite joined to the target's, below it where R4
/// is not nought.
pub(crate) fn append(os: &mut Os<'_>, target: &Target, regs: &Regs) -> Result<(), Fail> {
    let area = target.area;
    let one = target.sprite;
    let two = area.sprite(os, target.naming, regs.r[3])?;
    let below = regs.r[4] != 0;
    if one.at == two.at || one.mode != two.mode {
        return Err(os.error(CANNOT_JOIN));
    }
    let pixels = one.pixels_kept(os)?;
    two.still(os)?;
    area.holds(os, &one)?;
    let one_extents = one.whole(os, &pixels)?;
    area.holds(os, &two)?;
    let two_extents = two.whole(os, &pixels)?;
    let (w1, w2) = (one.width(&pixels), two.width(&pixels));
    let (h1, h2) = (one.rows(), two.rows());
    let shape = if below {
        (w1 == w2).then(|| h1.checked_add(h2).map(|rows| (w1, rows)))
    } else {
        (h1 == h2).then(|| w1.checked_add(w2).map(|width| (width, h1)))
    };
    let Some(Some((width, rows))) = shape else {
        return Err(os.error(CANNOT_JOIN));
    };
    let layout = Shape::new(&pixels, width, rows, 0);
    let masked = one.masked() || two.masked();
    let palette = one.image.min(one.mask) - SPRITE_HEADER_LEN;
    let image = layout.image.ok_or_else(|| os.error(NO_MEMORY))?;
    let mask = if masked {
        layout.mask.ok_or_else(|| os.error(NO_MEMORY))?
    } else {
        0
    };
    let length =
        u64::from(SPRITE_HEADER_LEN) + u64::from(palette) + u64::from(image) + u64::from(mask);
    let Ok(length) = u32::try_from(length) else {
        return Err(os.error(NO_MEMORY));
    };
    replaceable(os, area, [&one, &two], length)?;
    let mut body = fallible::zeroed((length - SPRITE_HEADER_LEN) as usize)
        .ok_or_else(|| os.error(NO_MEMORY))?;
    os.read_bytes(
        one.address(SPRITE_HEADER_LEN),
        &mut body[..palette as usize],
    )?;
    let joined = Joined {
        pixels,
        below,
        stride: u64::from(layout.words) * 4,
        width,
        w1,
        h1,
    };
    let sprites = [(&one, &one_extents), (&two, &two_extents)];
    let (image_part, mask_part) = body[palette as usize..].split_at_mut(image as usize);
    joined.rows(os, sprites, image_part, false)?;
    if masked {
        let mask_stride = u64::from(mask) / u64::from(rows.max(1));
        let joined = Joined {
            stride: mask_stride,
            ..joined
        };
        mask_part.fill(0xFF);
        joined.rows(os, sprites, mask_part, true)?;
    }
    let (later, earlier) = if two.at > one.at {
        (two, one)
    } else {
        (one, two)
    };
    area.remove(os, &later)?;
    area.remove(os, &earlier)?;
    let at = area.append(os, length, NO_MEMORY)?;
    let sprite = Sprite {
        at: area.address(at),
        next: length,
        words_less_one: layout.words.wrapping_sub(1),
        rows_less_one: rows.wrapping_sub(1),
        first_bit: 0,
        last_bit: layout.last_bit,
        image: SPRITE_HEADER_LEN + palette,
        mask: SPRITE_HEADER_LEN + palette + if masked { image } else { 0 },
        ..one
    };
    sprite.write(os)?;
    os.write_bytes(sprite.address(SPRITE_HEADER_LEN), &body)
}

/// Checks the area holds a sprite of `length` once both `sprites` are
/// removed, and memory takes the change from the earlier of them on: removing
/// them and appending it can then be done whole, so a refusal changes
/// nothing. Sprites overlapping each other cannot be removed apart.
fn replaceable(
    os: &mut Os<'_>,
    area: super::area::Area,
    [one, two]: [&Sprite; 2],
    length: u32,
) -> Result<(), Fail> {
    let block = area.block(os)?;
    let room =
        u64::from(block.size) - u64::from(block.free) + u64::from(one.next) + u64::from(two.next);
    if u64::from(length) > room {
        return Err(os.error(NO_MEMORY));
    }
    let end_of = |sprite: &Sprite| u64::from(sprite.at) + u64::from(sprite.next);
    let apart = end_of(one) <= u64::from(two.at) || end_of(two) <= u64::from(one.at);
    let freed_to = block
        .free
        .checked_sub(one.next)
        .and_then(|f| f.checked_sub(two.next));
    let (true, Some(freed_to)) = (apart, freed_to) else {
        return Err(os.error(super::UNRECOGNISED));
    };
    let touched_end = u64::from(block.free).max(u64::from(freed_to) + u64::from(length));
    if os.writable(one.at.min(two.at), u64::from(area.address(0)) + touched_end) {
        Ok(())
    } else {
        Err(os.error(super::UNRECOGNISED))
    }
}

/// How the joined sprite's rows are laid out.
#[derive(Clone, Copy)]
struct Joined {
    pixels: Pixels,
    below: bool,
    /// The bytes each of the block's rows takes.
    stride: u64,
    width: u32,
    /// The first sprite's width and height.
    w1: u32,
    h1: u32,
}

impl Joined {
    /// Fills `out` with the joined image's rows, or its mask's where `mask`,
    /// from the two sprites', each proven whole.
    fn rows(
        &self,
        os: &mut Os<'_>,
        sprites: [(&Sprite, &Extents); 2],
        out: &mut [u8],
        mask: bool,
    ) -> Result<(), Fail> {
        if out.is_empty() {
            return Ok(());
        }
        let bits = if mask && !self.pixels.old_format {
            self.pixels.mask_bits()
        } else {
            self.pixels.field_bits()
        };
        let blocks = sprites.map(|(sprite, extents)| {
            if mask {
                (sprite.mask, extents.mask_stride)
            } else {
                (sprite.image, extents.stride)
            }
        });
        let mut sources = [alloc::vec::Vec::new(), alloc::vec::Vec::new()];
        for ((source, &(_, stride)), (sprite, _)) in sources.iter_mut().zip(&blocks).zip(sprites) {
            if sprite.rows() > 0 {
                *source = row_buffer(os, stride)?;
            }
        }
        let opaque = u32::try_from((1u64 << bits) - 1).unwrap_or(u32::MAX);
        let stride = usize::try_from(self.stride).map_err(|_| os.error(NO_MEMORY))?;
        for (row, line) in (0u32..).zip(out.chunks_exact_mut(stride.max(1))) {
            os.yield_point()?;
            for (which, (sprite, _)) in sprites.into_iter().enumerate() {
                let source_row = if self.below {
                    if (which == 0) != (row < self.h1) {
                        continue;
                    }
                    if which == 0 {
                        row
                    } else {
                        row - self.h1
                    }
                } else {
                    row
                };
                let (start, row_stride) = blocks[which];
                let has = !mask || sprite.masked();
                if has {
                    let address =
                        advanced(sprite.address(start), row_stride * u64::from(source_row));
                    os.read_bytes(address, &mut sources[which])?;
                }
                let (from_x, to_x) = match (self.below, which) {
                    (true, _) => (0, self.width),
                    (false, 0) => (0, self.w1),
                    (false, _) => (self.w1, self.width),
                };
                let source_offset = if mask && !self.pixels.old_format {
                    0
                } else {
                    u64::from(sprite.first_bit)
                };
                for x in from_x..to_x {
                    let value = if has {
                        let sx = u64::from(x - from_x);
                        field_at(&sources[which], source_offset + sx * u64::from(bits), bits)
                    } else {
                        opaque
                    };
                    set_field_at(line, u64::from(x) * u64::from(bits), bits, value);
                }
            }
        }
        Ok(())
    }
}
