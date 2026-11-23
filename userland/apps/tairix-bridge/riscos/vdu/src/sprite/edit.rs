//! Changing a sprite's shape (`OS_SpriteOp` 29 to 33, 37, 45 to 47, 54, 57
//! and 58): its mask made or removed, rows and columns inserted or deleted,
//! flips, its left-hand wastage taken out, its palette made or removed. Each
//! works through guest memory a row at a time, the area's sprites after the
//! sprite moved as it grows or shrinks.

use alloc::vec::Vec;

use tairix_image::{SPRITE_HEADER_LEN, SPRITE_PALETTE_ENTRY_LEN};
use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};
use tairix_util::bitfield::{field_at, set_field_at};

use super::create::{new_palette_entries, Shape, Target};
use super::format::Pixels;
use super::header::{advanced, Extents, Sprite};
use super::{FULL_PALETTE, NO_DEEP_PALETTE, NO_MEMORY, NO_ROOM_TO_GROW, OUTSIDE, UNRECOGNISED};
use crate::palette::{default_words, grouped_colours, GROUP_BASES};

/// A palette word's low byte, the form a colour set for both flash states
/// takes, as RISC OS 5 writes every palette entry it makes.
const BOTH_STATES: u32 = 0x10;

/// `bytes` zero bytes for a row, borrowed from the heap fallibly: "Not
/// enough memory" where the host has no room for them.
pub(crate) fn row_buffer(os: &mut Os<'_>, bytes: u64) -> Result<Vec<u8>, Fail> {
    usize::try_from(bytes)
        .ok()
        .and_then(tairix_util::fallible::zeroed)
        .ok_or_else(|| os.error(NO_MEMORY))
}

/// The first `bytes` of `buffer`, which was made at least that long.
pub(crate) fn first_bytes(buffer: &mut [u8], bytes: u64) -> &mut [u8] {
    let length = usize::try_from(bytes).map_or(buffer.len(), |bytes| bytes.min(buffer.len()));
    &mut buffer[..length]
}

/// `count` as a byte count within a word; "Not enough memory" where it is
/// not.
pub(crate) fn bytes32(os: &mut Os<'_>, count: u64) -> Result<u32, Fail> {
    u32::try_from(count).map_err(|_| os.error(NO_MEMORY))
}

/// A block of a sprite a change rewrites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    Image,
    Mask,
}

impl Target {
    /// Opens `bytes` at the sprite's byte `offset`, its length growing.
    pub(crate) fn open(
        &mut self,
        os: &mut Os<'_>,
        offset: u32,
        bytes: u32,
        no_room: Error,
    ) -> Result<(), Fail> {
        let at = self.area.offset_of(&self.sprite, offset);
        self.area.open(os, at, bytes, no_room)?;
        self.sprite.next = self.sprite.next.wrapping_add(bytes);
        Ok(())
    }

    /// Closes `bytes` at the sprite's byte `offset`, its length shrinking.
    pub(crate) fn close(&mut self, os: &mut Os<'_>, offset: u32, bytes: u32) -> Result<(), Fail> {
        let at = self.area.offset_of(&self.sprite, offset);
        self.area.close(os, at, bytes)?;
        self.sprite.next = self.sprite.next.wrapping_sub(bytes);
        Ok(())
    }

    /// The sprite checked whole within its area and within itself, memory
    /// backing all of it and taking a change to it: its pixels and extents,
    /// before a change to its shape.
    pub(crate) fn reshapeable(&self, os: &mut Os<'_>) -> Result<(Pixels, Extents), Fail> {
        let pixels = self.sprite.pixels_kept(os)?;
        self.area.holds(os, &self.sprite)?;
        let extents = self.sprite.changeable(os, &pixels)?;
        Ok((pixels, extents))
    }

    /// Checks the area holds `bytes` more and memory takes a change from the
    /// sprite to the end of what the area would then hold: what a change
    /// opening room at or past the sprite, perhaps in more than one step,
    /// checks first, so that a refusal leaves everything as it was.
    pub(crate) fn can_grow(&self, os: &mut Os<'_>, bytes: u64, no_room: Error) -> Result<(), Fail> {
        let block = self.area.block(os)?;
        let free = u64::from(block.free).saturating_add(bytes);
        if free > u64::from(block.size) {
            return Err(os.error(no_room));
        }
        if !os.writable(self.sprite.at, u64::from(self.area.address(0)) + free) {
            return Err(os.error(UNRECOGNISED));
        }
        Ok(())
    }

    /// Rewrites the block of `rows` rows of `old` bytes each at the sprite's
    /// byte `at` as rows of `new` bytes, each made by `make` from the old in
    /// `from` and `to`, buffers of `old` and `new` bytes; the difference
    /// opened or closed at the block's end, and whichever of the image and
    /// mask lie past the block moved with it. Its caller has checked the area
    /// holds the change, so only the host stopping it leaves it part made.
    fn restride(
        &mut self,
        os: &mut Os<'_>,
        block: Block,
        rows: u32,
        (old, new): (u64, u64),
        (from, to): (&mut [u8], &mut [u8]),
        mut make: impl FnMut(&[u8], &mut [u8]),
    ) -> Result<(), Fail> {
        if rows == 0 {
            return Ok(());
        }
        let at = match block {
            Block::Image => self.sprite.image,
            Block::Mask => self.sprite.mask,
        };
        let rows64 = u64::from(rows);
        let old_bytes = old * rows64;
        let new_bytes = new
            .checked_mul(rows64)
            .ok_or_else(|| os.error(NO_ROOM_TO_GROW))?;
        let mut each = |os: &mut Os<'_>, row: u32, sprite: &Sprite| -> Result<(), Fail> {
            let base = sprite.address(at);
            os.read_bytes(advanced(base, old * u64::from(row)), from)?;
            to.fill(0);
            make(from, to);
            os.write_bytes(advanced(base, new * u64::from(row)), to)?;
            Ok(os.yield_point()?)
        };
        let end = bytes32(os, u64::from(at) + old_bytes)?;
        if new > old {
            let grown = bytes32(os, new_bytes - old_bytes)?;
            self.open(os, end, grown, NO_ROOM_TO_GROW)?;
            self.past(end, block, |offset| offset.wrapping_add(grown));
            for row in (0..rows).rev() {
                each(os, row, &self.sprite)?;
            }
        } else {
            let shrunk = bytes32(os, old_bytes - new_bytes)?;
            for row in 0..rows {
                each(os, row, &self.sprite)?;
            }
            let kept = end - shrunk;
            self.close(os, kept, shrunk)?;
            self.past(end, block, |offset| offset.wrapping_sub(shrunk));
        }
        Ok(())
    }

    /// Moves the image and mask offsets at or past `from` by `moved`, but for
    /// that of `changed`, the block rewritten where it lies: one of no bytes
    /// ends where it starts.
    pub(crate) fn past(&mut self, from: u32, changed: Block, moved: impl Fn(u32) -> u32) {
        let masked = self.sprite.masked();
        if changed != Block::Image && self.sprite.image >= from {
            self.sprite.image = moved(self.sprite.image);
        }
        if !masked {
            self.sprite.mask = self.sprite.image;
        } else if changed != Block::Mask && self.sprite.mask >= from {
            self.sprite.mask = moved(self.sprite.mask);
        }
    }

    /// Writes the control block as it now stands.
    pub(crate) fn finish(&self, os: &mut Os<'_>) -> Result<(), Fail> {
        self.sprite.write(os)
    }

    /// Gives the sprite a mask of `bytes` after its image of `image_bytes`,
    /// every bit set.
    pub(crate) fn add_mask(
        &mut self,
        os: &mut Os<'_>,
        image_bytes: u64,
        bytes: u32,
    ) -> Result<(), Fail> {
        let at = bytes32(os, u64::from(self.sprite.image) + image_bytes)?;
        self.open(os, at, bytes, NO_MEMORY)?;
        if self.sprite.image >= at {
            self.sprite.image = self.sprite.image.wrapping_add(bytes);
        }
        self.sprite.mask = at;
        os.fill(self.sprite.address(at), bytes, 0xFF)
    }

    /// Takes away the sprite's mask of `bytes`.
    pub(crate) fn drop_mask(&mut self, os: &mut Os<'_>, bytes: u32) -> Result<(), Fail> {
        let at = self.sprite.mask;
        self.close(os, at, bytes)?;
        if self.sprite.image > at {
            self.sprite.image = self.sprite.image.wrapping_sub(bytes);
        }
        self.sprite.mask = self.sprite.image;
        Ok(())
    }
}

/// `OS_SpriteOp` 29: a mask, every pixel opaque, after the image; an existing
/// mask made opaque throughout.
pub(crate) fn create_mask(os: &mut Os<'_>, target: &Target) -> Result<(), Fail> {
    let mut target = *target;
    let (_, extents) = target.reshapeable(os)?;
    let bytes = bytes32(os, extents.mask_bytes)?;
    if target.sprite.masked() {
        return os.fill(target.sprite.address(target.sprite.mask), bytes, 0xFF);
    }
    target.add_mask(os, extents.image_bytes, bytes)?;
    target.finish(os)
}

/// `OS_SpriteOp` 30: the mask removed, if there is one.
pub(crate) fn remove_mask(os: &mut Os<'_>, target: &Target) -> Result<(), Fail> {
    let mut target = *target;
    if !target.sprite.masked() {
        return Ok(());
    }
    let (_, extents) = target.reshapeable(os)?;
    let bytes = bytes32(os, extents.mask_bytes)?;
    target.drop_mask(os, bytes)?;
    target.finish(os)
}

/// `OS_SpriteOp` 33: upside down, the mask with it.
pub(crate) fn flip_rows(os: &mut Os<'_>, target: &Target) -> Result<(), Fail> {
    let sprite = &target.sprite;
    let pixels = sprite.pixels_kept(os)?;
    let extents = sprite.changeable(os, &pixels)?;
    let rows = sprite.rows();
    if rows < 2 {
        return Ok(());
    }
    let mask_stride = if sprite.masked() {
        extents.mask_stride
    } else {
        0
    };
    let widest = extents.stride.max(mask_stride);
    let mut top = row_buffer(os, widest)?;
    let mut bottom = row_buffer(os, widest)?;
    let mut flip = |os: &mut Os<'_>, start: u32, stride: u64| {
        flip_block(
            os,
            sprite.address(start),
            rows,
            stride,
            (
                first_bytes(&mut top, stride),
                first_bytes(&mut bottom, stride),
            ),
        )
    };
    flip(os, sprite.image, extents.stride)?;
    if sprite.masked() {
        flip(os, sprite.mask, mask_stride)?;
    }
    Ok(())
}

/// Exchanges the first and last of `rows` rows of `stride` bytes at
/// `address`, and so on inwards, through `top` and `bottom`, each a row.
fn flip_block(
    os: &mut Os<'_>,
    address: u32,
    rows: u32,
    stride: u64,
    (top, bottom): (&mut [u8], &mut [u8]),
) -> Result<(), Fail> {
    for row in 0..rows / 2 {
        let (upper, lower) = (
            advanced(address, stride * u64::from(row)),
            advanced(address, stride * u64::from(rows - 1 - row)),
        );
        os.read_bytes(upper, top)?;
        os.read_bytes(lower, bottom)?;
        os.write_bytes(upper, bottom)?;
        os.write_bytes(lower, top)?;
        os.yield_point()?;
    }
    Ok(())
}

/// `OS_SpriteOp` 47: left to right, the mask with it. An old-format sprite's
/// whole stored row is mirrored, its wastage moving to the other side; a new
/// one's pixels are mirrored within its width and its mask's rows a bit at a
/// time across its pixels' bits, the rest of each row cleared, as RISC OS 5
/// mirrors them — which reverses each value of an eight-bit mask's bits.
pub(crate) fn flip_columns(os: &mut Os<'_>, target: &Target) -> Result<(), Fail> {
    let mut target = *target;
    let sprite = target.sprite;
    let pixels = sprite.pixels_kept(os)?;
    let extents = sprite.changeable(os, &pixels)?;
    let field = pixels.field_bits();
    let image = if pixels.old_format {
        Mirror::Fields {
            count: sprite.words().wrapping_mul(32) / field,
            bits: field,
            clear_rest: false,
        }
    } else {
        Mirror::Fields {
            count: sprite.width(&pixels),
            bits: field,
            clear_rest: true,
        }
    };
    let mask = if pixels.old_format {
        image
    } else {
        Mirror::Bits {
            used: (u64::from(sprite.width(&pixels)) * u64::from(pixels.mask_bits()))
                .min(extents.mask_stride.saturating_mul(8)),
        }
    };
    if sprite.rows() > 0 {
        let mask_stride = if sprite.masked() {
            extents.mask_stride
        } else {
            0
        };
        let mut row = row_buffer(os, extents.stride.max(mask_stride))?;
        let reversed_bytes = match mask {
            Mirror::Bits { used } if sprite.masked() => used.div_ceil(8),
            _ => 0,
        };
        let mut reversed = row_buffer(os, reversed_bytes)?;
        let mut mirror = |os: &mut Os<'_>, at: u32, stride: u64, how: Mirror| {
            mirror_block(
                os,
                &sprite,
                at,
                how,
                (first_bytes(&mut row, stride), &mut reversed),
            )
        };
        mirror(os, sprite.image, extents.stride, image)?;
        if sprite.masked() {
            mirror(os, sprite.mask, mask_stride, mask)?;
        }
    }
    if pixels.old_format {
        target.sprite.first_bit = 31u32.wrapping_sub(sprite.last_bit) & 31;
        target.sprite.last_bit = 31u32.wrapping_sub(sprite.first_bit) & 31;
        target.finish(os)?;
    }
    Ok(())
}

/// How a block's rows are mirrored.
#[derive(Clone, Copy)]
enum Mirror {
    /// Their first `count` fields of `bits` each, every bit past them
    /// cleared where `clear_rest`.
    Fields {
        count: u32,
        bits: u32,
        clear_rest: bool,
    },
    /// Their first `used` bits, each bit a field, every bit past them cleared;
    /// no more than a row holds.
    Bits { used: u64 },
}

/// Mirrors every row of the block at the sprite's byte `at`, each read into
/// `row`, the block's rows' length, `reversed` room for a mirrored row's used
/// bits.
fn mirror_block(
    os: &mut Os<'_>,
    sprite: &Sprite,
    at: u32,
    mirror: Mirror,
    (row, reversed): (&mut [u8], &mut [u8]),
) -> Result<(), Fail> {
    let stride = row.len() as u64;
    let bits_used = match mirror {
        Mirror::Bits { used } => used,
        Mirror::Fields { .. } => 0,
    };
    let reversed = &mut reversed[..usize::try_from(bits_used.div_ceil(8)).unwrap_or(0)];
    for index in 0..sprite.rows() {
        let address = advanced(sprite.address(at), stride * u64::from(index));
        os.read_bytes(address, row)?;
        match mirror {
            Mirror::Fields {
                count,
                bits,
                clear_rest,
            } => {
                for p in 0..count / 2 {
                    let q = count - 1 - p;
                    let (a, b) = (field(row, p, bits), field(row, q, bits));
                    set_field(row, p, bits, b);
                    set_field(row, q, bits, a);
                }
                if clear_rest {
                    clear_from(row, u64::from(count) * u64::from(bits));
                }
            }
            Mirror::Bits { .. } => reverse_bits(row, bits_used, reversed),
        }
        os.write_bytes(address, row)?;
        os.yield_point()?;
    }
    Ok(())
}

/// Reverses the order of `row`'s first `used` bits, `reversed` being room
/// for their bytes, and clears every bit past them.
fn reverse_bits(row: &mut [u8], used: u64, reversed: &mut [u8]) {
    let bytes = reversed.len();
    for (out, byte) in reversed.iter_mut().zip(row[..bytes].iter().rev()) {
        *out = byte.reverse_bits();
    }
    let spare = u32::try_from((8 - used % 8) % 8).unwrap_or(0);
    for (k, out) in row[..bytes].iter_mut().enumerate() {
        let next = reversed.get(k + 1).copied().unwrap_or(0);
        let pair = u16::from_le_bytes([reversed[k], next]);
        *out = (pair >> spare).to_le_bytes()[0];
    }
    clear_from(row, used);
}

/// Clears every bit of `row` from bit `bit` on.
fn clear_from(row: &mut [u8], bit: u64) {
    let first = usize::try_from(bit / 8).unwrap_or(usize::MAX);
    if let Some(byte) = row.get_mut(first) {
        *byte &= (1u8 << (bit % 8)).wrapping_sub(1);
    }
    for byte in row.iter_mut().skip(first.saturating_add(1)) {
        *byte = 0;
    }
}

/// Pixel `index` of `bits` bits along `row`, from its first bit.
pub(crate) fn field(row: &[u8], index: u32, bits: u32) -> u32 {
    field_at(row, u64::from(index) * u64::from(bits), bits)
}

/// Sets pixel `index` of `bits` bits along `row` to `value`.
pub(crate) fn set_field(row: &mut [u8], index: u32, bits: u32, value: u32) {
    set_field_at(row, u64::from(index) * u64::from(bits), bits, value);
}

/// `OS_SpriteOp` 54: an old-format sprite's rows moved down to bit nought,
/// a word fewer where its right-hand wastage then reaches one; nothing for a
/// new-format sprite, which has none.
///
/// RISC OS 5 shifts the next word's bits into the wastage on the right; they
/// are nought here, which no plot shows.
pub(crate) fn remove_wastage(os: &mut Os<'_>, target: &Target) -> Result<(), Fail> {
    let mut target = *target;
    let (pixels, extents) = target.reshapeable(os)?;
    let shift = target.sprite.first_bit;
    if !pixels.old_format || shift == 0 {
        return Ok(());
    }
    let sprite = target.sprite;
    let used = sprite
        .words_less_one
        .wrapping_mul(32)
        .wrapping_add(sprite.last_bit)
        .wrapping_add(1)
        .wrapping_sub(shift);
    let words = used.div_ceil(32);
    let (old, new) = (extents.stride, 4 * u64::from(words));
    let moved = move |from: &[u8], to: &mut [u8]| {
        for bit in (0..u64::from(used)).step_by(8) {
            let take = u32::try_from((u64::from(used) - bit).min(8)).unwrap_or(8);
            set_field_at(to, bit, take, field_at(from, bit + u64::from(shift), take));
        }
    };
    let masked = sprite.masked();
    if sprite.rows() > 0 {
        target.can_grow(os, 0, NO_ROOM_TO_GROW)?;
        let mut from = row_buffer(os, old)?;
        let mut to = row_buffer(os, new)?;
        target.restride(
            os,
            Block::Image,
            sprite.rows(),
            (old, new),
            (&mut from, &mut to),
            moved,
        )?;
        if masked {
            target.restride(
                os,
                Block::Mask,
                sprite.rows(),
                (old, new),
                (&mut from, &mut to),
                moved,
            )?;
        }
    }
    target.sprite.words_less_one = words.wrapping_sub(1);
    target.sprite.first_bit = 0;
    target.sprite.last_bit = used.wrapping_sub(1) & 31;
    target.finish(os)
}

/// How many rows or columns to insert or delete, and where.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// `count` before row or column `at`.
    Insert { at: u32, count: u32 },
    /// `count` from row or column `at` on.
    Delete { at: u32, count: u32 },
}

impl Change {
    /// What `OS_SpriteOp` 57 and 58 ask: R4 rows or columns inserted before
    /// R3, or where negative deleted from R3 on.
    pub(crate) const fn of(regs: &Regs) -> Self {
        let count = regs.r[4].cast_signed();
        if count >= 0 {
            Self::Insert {
                at: regs.r[3],
                count: count.cast_unsigned(),
            }
        } else {
            Self::Delete {
                at: regs.r[3],
                count: count.unsigned_abs(),
            }
        }
    }

    /// Whether it fits a sprite of `size` rows or columns: an insertion at
    /// or before its end, a deletion of ones it has that leaves at least one.
    const fn fits(self, size: u32) -> bool {
        match self {
            Self::Insert { at, .. } => at <= size,
            Self::Delete { at, count } => match at.checked_add(count) {
                Some(end) => end <= size && count < size,
                None => false,
            },
        }
    }
}

/// `OS_SpriteOp` 31, 32 and 57: rows inserted, colour nought and transparent,
/// or deleted, rows counted from the bottom.
pub(crate) fn rows(os: &mut Os<'_>, target: &Target, change: Change) -> Result<(), Fail> {
    let mut target = *target;
    let (_, extents) = target.reshapeable(os)?;
    let sprite = target.sprite;
    let rows = sprite.rows();
    if !change.fits(rows) {
        return Err(os.error(OUTSIDE));
    }
    let masked = sprite.masked();
    let per_row = extents.stride + if masked { extents.mask_stride } else { 0 };
    let (Change::Insert { count, .. } | Change::Delete { count, .. }) = change;
    let moved = per_row
        .checked_mul(u64::from(count))
        .ok_or_else(|| os.error(NO_ROOM_TO_GROW))?;
    let grows = if matches!(change, Change::Insert { .. }) {
        moved
    } else {
        0
    };
    target.can_grow(os, grows, NO_ROOM_TO_GROW)?;
    let blocks: [(bool, u64); 2] = [(false, extents.stride), (true, extents.mask_stride)];
    for (is_mask, stride) in blocks {
        if is_mask && !target.sprite.masked() {
            continue;
        }
        let start = if is_mask {
            target.sprite.mask
        } else {
            target.sprite.image
        };
        match change {
            Change::Insert { at, count } => {
                let place = bytes32(os, u64::from(start) + stride * u64::from(rows - at))?;
                let bytes = bytes32(os, stride * u64::from(count))?;
                target.open(os, place, bytes, NO_ROOM_TO_GROW)?;
                os.fill(target.sprite.address(place), bytes, 0)?;
                shift_other(&mut target, is_mask, place, |o| o.wrapping_add(bytes));
            }
            Change::Delete { at, count } => {
                let place = bytes32(os, u64::from(start) + stride * u64::from(rows - at - count))?;
                let bytes = bytes32(os, stride * u64::from(count))?;
                target.close(os, place, bytes)?;
                shift_other(&mut target, is_mask, place + 1, |o| o.wrapping_sub(bytes));
            }
        }
    }
    target.sprite.rows_less_one = match change {
        Change::Insert { count, .. } => sprite.rows_less_one.wrapping_add(count),
        Change::Delete { count, .. } => sprite.rows_less_one.wrapping_sub(count),
    };
    target.finish(os)
}

/// After the image or mask block changed size at `place`, moves the other's
/// offset where it lies at or past `place`.
fn shift_other(target: &mut Target, is_mask: bool, place: u32, moved: impl Fn(u32) -> u32) {
    let masked = target.sprite.masked();
    if is_mask {
        if target.sprite.image >= place {
            target.sprite.image = moved(target.sprite.image);
        }
    } else {
        if masked && target.sprite.mask >= place {
            target.sprite.mask = moved(target.sprite.mask);
        }
        if !masked {
            target.sprite.mask = target.sprite.image;
        }
    }
}

/// `OS_SpriteOp` 45, 46 and 58: columns inserted, colour nought and
/// transparent, or deleted, columns counted from the left. An old-format
/// sprite's pixels keep their bits, its rows taking more words as they need;
/// a new-format mask's rows hold its pixels alone.
pub(crate) fn columns(os: &mut Os<'_>, target: &Target, change: Change) -> Result<(), Fail> {
    let mut target = *target;
    let (pixels, extents) = target.reshapeable(os)?;
    let sprite = target.sprite;
    let width = sprite.width(&pixels);
    if !change.fits(width) {
        return Err(os.error(OUTSIDE));
    }
    let new_width = match change {
        Change::Insert { count, .. } => width.checked_add(count),
        Change::Delete { count, .. } => width.checked_sub(count),
    };
    let Some(new_width) = new_width else {
        return Err(os.error(NO_ROOM_TO_GROW));
    };
    let field = pixels.field_bits();
    let first = u64::from(sprite.first_bit);
    let rows = sprite.rows();
    let shape = Shape::new(&pixels, new_width, rows, sprite.first_bit);
    let new_stride = 4 * u64::from(shape.words);
    let masked = sprite.masked();
    let (old_mask_stride, new_mask_stride) = match (masked, shape.mask_stride) {
        (false, _) => (0, 0),
        (true, Some(new)) => (extents.mask_stride, new),
        (true, None) => return Err(os.error(NO_ROOM_TO_GROW)),
    };
    let old_bytes = (extents.stride + old_mask_stride) * u64::from(rows);
    let new_bytes = (new_stride + new_mask_stride)
        .checked_mul(u64::from(rows))
        .ok_or_else(|| os.error(NO_ROOM_TO_GROW))?;
    target.can_grow(os, new_bytes.saturating_sub(old_bytes), NO_ROOM_TO_GROW)?;
    let regrouped = move |offset: u64, bits: u32| {
        move |from: &[u8], to: &mut [u8]| {
            for x in 0..new_width {
                let source = match change {
                    Change::Insert { at, count } if x >= at && x < at + count => None,
                    Change::Insert { at, count } if x >= at => Some(x - count),
                    Change::Delete { at, count } if x >= at => Some(x + count),
                    _ => Some(x),
                };
                if let Some(old) = source {
                    let value = field_at(from, offset + u64::from(old) * u64::from(bits), bits);
                    set_field_at(to, offset + u64::from(x) * u64::from(bits), bits, value);
                }
            }
        }
    };
    if rows > 0 {
        let mut from = row_buffer(os, extents.stride.max(old_mask_stride))?;
        let mut to = row_buffer(os, new_stride.max(new_mask_stride))?;
        target.restride(
            os,
            Block::Image,
            rows,
            (extents.stride, new_stride),
            (
                first_bytes(&mut from, extents.stride),
                first_bytes(&mut to, new_stride),
            ),
            regrouped(first, field),
        )?;
        if masked {
            let (offset, mask_bits) = if pixels.old_format {
                (first, field)
            } else {
                (0, pixels.mask_bits())
            };
            target.restride(
                os,
                Block::Mask,
                rows,
                (old_mask_stride, new_mask_stride),
                (
                    first_bytes(&mut from, old_mask_stride),
                    first_bytes(&mut to, new_mask_stride),
                ),
                regrouped(offset, mask_bits),
            )?;
        }
    }
    target.sprite.words_less_one = shape.words.wrapping_sub(1);
    target.sprite.last_bit = shape.last_bit;
    target.finish(os)
}

/// `OS_SpriteOp` 37: R3 -1 reads the palette, R3 its entries, R4 its
/// address and R5 the mode, nought for none; nought removes it; otherwise a
/// palette is made, an eight-bit sprite's of 256 entries where R3's top bit
/// asks, else sixty-four.
pub(crate) fn palette(os: &mut Os<'_>, target: &Target, regs: &mut Regs) -> Result<(), Fail> {
    let mut target = *target;
    let held = target.sprite.palette_entries();
    match regs.r[3] {
        u32::MAX => {
            regs.r[3] = held;
            regs.r[4] = if held == 0 {
                0
            } else {
                target.sprite.address(SPRITE_HEADER_LEN)
            };
            regs.r[5] = target.sprite.mode;
            return Ok(());
        }
        0 => {
            if held != 0 {
                let _ = target.reshapeable(os)?;
                resize_palette(os, &mut target, held, 0)?;
            }
            return Ok(());
        }
        _ => {}
    }
    let (pixels, _) = target.reshapeable(os)?;
    if !pixels.paletted() {
        return Err(os.error(NO_DEEP_PALETTE));
    }
    let entries = if pixels.log2_bpp == 3 {
        if regs.r[3] & super::reason::PALETTE_FULL != 0 {
            FULL_PALETTE
        } else {
            new_palette_entries(&pixels)
        }
    } else if held == 0 {
        new_palette_entries(&pixels)
    } else {
        held
    };
    let colours = palette_colours(os, &target.sprite, &pixels, held, entries)?;
    resize_palette(os, &mut target, held, entries)?;
    let mut bytes = Vec::new();
    if bytes
        .try_reserve_exact(colours.len() * SPRITE_PALETTE_ENTRY_LEN as usize)
        .is_err()
    {
        return Err(os.error(NO_MEMORY));
    }
    for colour in colours {
        bytes.extend_from_slice(&colour.to_le_bytes());
        bytes.extend_from_slice(&colour.to_le_bytes());
    }
    os.write_bytes(target.sprite.address(SPRITE_HEADER_LEN), &bytes)
}

/// The words of the `entries` a palette made for the sprite holds, as RISC OS
/// 5 makes them: the depth's default where there was none; where there was,
/// each entry's first flash state kept in both, an eight-bit palette rebuilt
/// as VIDC1 would show its first sixteen entries' colours. Every word's low
/// byte gains the both-states form; a rebuilt one keeps its base's low byte,
/// its top four bits copied into its bottom four, as RISC OS 5 leaves it.
fn palette_colours(
    os: &mut Os<'_>,
    sprite: &Sprite,
    pixels: &Pixels,
    held: u32,
    entries: u32,
) -> Result<Vec<u32>, Fail> {
    let mut colours = Vec::new();
    if colours.try_reserve_exact(entries as usize).is_err() {
        return Err(os.error(NO_MEMORY));
    }
    let grouped = held != 0 && pixels.log2_bpp == 3;
    let read = if grouped {
        held.min(len32(GROUP_BASES))
    } else if held != 0 {
        entries
    } else {
        0
    };
    let held_pairs = os.read_vec(
        sprite.address(SPRITE_HEADER_LEN),
        read as usize * SPRITE_PALETTE_ENTRY_LEN as usize,
    )?;
    let mut firsts = held_pairs
        .as_chunks::<8>()
        .0
        .iter()
        .map(|pair| u32::from_le_bytes([pair[0], pair[1], pair[2], pair[3]]));
    if grouped {
        let mut bases = [0; GROUP_BASES];
        for (base, first) in bases.iter_mut().zip(&mut firsts) {
            *base = first;
        }
        for (shown, i) in grouped_colours(&bases).zip(0..entries) {
            let [low, ..] = bases[i as usize % GROUP_BASES].to_le_bytes();
            colours.push(shown.riscos_word() | BOTH_STATES | u32::from(low | low >> 4));
        }
    } else if held != 0 {
        colours.extend(firsts.map(|first| first | BOTH_STATES));
    } else {
        let defaults = default_words(pixels.log2_bpp, false);
        colours.extend(
            defaults
                .iter()
                .take(entries as usize)
                .map(|&[first, _]| first | BOTH_STATES),
        );
    }
    Ok(colours)
}

/// Grows or shrinks the palette from `held` entries to `entries`, the image
/// and mask moving with it.
fn resize_palette(
    os: &mut Os<'_>,
    target: &mut Target,
    held: u32,
    entries: u32,
) -> Result<(), Fail> {
    let at = SPRITE_HEADER_LEN + held.min(entries) * SPRITE_PALETTE_ENTRY_LEN;
    if entries > held {
        let bytes = (entries - held) * SPRITE_PALETTE_ENTRY_LEN;
        target.open(os, at, bytes, NO_MEMORY)?;
        target.sprite.image = target.sprite.image.wrapping_add(bytes);
        target.sprite.mask = target.sprite.mask.wrapping_add(bytes);
    } else if held > entries {
        let bytes = (held - entries) * SPRITE_PALETTE_ENTRY_LEN;
        target.close(os, at, bytes)?;
        target.sprite.image = target.sprite.image.wrapping_sub(bytes);
        target.sprite.mask = target.sprite.mask.wrapping_sub(bytes);
    }
    target.finish(os)
}
