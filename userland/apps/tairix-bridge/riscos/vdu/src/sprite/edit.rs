//! Changing a sprite's shape (`OS_SpriteOp` 29 to 33, 37, 45 to 47, 54, 57
//! and 58): its mask made or removed, rows and columns inserted or deleted,
//! flips, its left-hand wastage taken out, its palette made or removed. Each
//! works through guest memory a row at a time, the area's sprites after the
//! sprite moved as it grows or shrinks.

use alloc::vec::Vec;

use tairix_image::{SPRITE_HEADER_LEN, SPRITE_PALETTE_ENTRY_LEN};
use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_vdu::palette::{default_256, grouped, DEFAULT_2, DEFAULT_4};

use super::create::{new_palette_entries, Target};
use super::format::Pixels;
use super::header::{advanced, Sprite};
use super::{FULL_PALETTE, NO_DEEP_PALETTE, NO_MEMORY, NO_ROOM_TO_GROW, OUTSIDE};

/// A palette word's low byte, the form a colour set for both flash states
/// takes, as RISC OS 5 writes every palette entry it makes.
const BOTH_STATES: u32 = 0x10;

/// A row's bytes, borrowed from the heap fallibly.
pub(crate) fn row_buffer(os: &mut Os<'_>, bytes: u64) -> Result<Vec<u8>, Fail> {
    let length = usize::try_from(bytes).map_err(|_| os.error(NO_MEMORY))?;
    let mut row = Vec::new();
    if row.try_reserve_exact(length).is_err() {
        return Err(os.error(NO_MEMORY));
    }
    row.resize(length, 0);
    Ok(row)
}

/// `count` as a byte count within a word; "Not enough memory" where it is
/// not.
pub(crate) fn bytes32(os: &mut Os<'_>, count: u64) -> Result<u32, Fail> {
    u32::try_from(count).map_err(|_| os.error(NO_MEMORY))
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

    /// The sprite checked whole within its area and within itself, with its
    /// pixels, before a change to its shape.
    pub(crate) fn reshapeable(&self, os: &mut Os<'_>) -> Result<Pixels, Fail> {
        self.area.holds(os, &self.sprite)?;
        let pixels = self.sprite.pixels(os)?;
        self.sprite.whole(os, &pixels)?;
        Ok(pixels)
    }

    /// Rewrites the block of `rows` rows of `old` bytes each at the sprite's
    /// byte `at` as rows of `new` bytes, each made by `make` from the old,
    /// the difference opened or closed at the block's end, and whichever of
    /// the image and mask lie past the block moved with it.
    fn restride(
        &mut self,
        os: &mut Os<'_>,
        at: u32,
        rows: u32,
        (old, new): (u64, u64),
        mut make: impl FnMut(&[u8], &mut [u8]),
    ) -> Result<(), Fail> {
        let mut from = row_buffer(os, old)?;
        let mut to = row_buffer(os, new)?;
        let end = u64::from(at) + old * u64::from(rows);
        let mut each = |os: &mut Os<'_>, row: u32, sprite: &Sprite| -> Result<(), Fail> {
            let base = sprite.address(at);
            os.read_bytes(advanced(base, old * u64::from(row)), &mut from)?;
            to.fill(0);
            make(&from, &mut to);
            os.write_bytes(advanced(base, new * u64::from(row)), &to)
        };
        if new > old {
            let grown = bytes32(os, (new - old) * u64::from(rows))?;
            let end = bytes32(os, end)?;
            self.open(os, end, grown, NO_ROOM_TO_GROW)?;
            self.past(end, |offset| offset.wrapping_add(grown));
            for row in (0..rows).rev() {
                each(os, row, &self.sprite)?;
            }
        } else {
            let shrunk = bytes32(os, (old - new) * u64::from(rows))?;
            for row in 0..rows {
                each(os, row, &self.sprite)?;
            }
            let kept = bytes32(os, new * u64::from(rows) + u64::from(at))?;
            self.close(os, kept, shrunk)?;
            self.past(kept + shrunk, |offset| offset.wrapping_sub(shrunk));
        }
        Ok(())
    }

    /// Moves the image and mask offsets at or past `from` by `moved`.
    pub(crate) fn past(&mut self, from: u32, moved: impl Fn(u32) -> u32) {
        let masked = self.sprite.masked();
        if self.sprite.image >= from {
            self.sprite.image = moved(self.sprite.image);
        }
        if !masked {
            self.sprite.mask = self.sprite.image;
        } else if self.sprite.mask >= from {
            self.sprite.mask = moved(self.sprite.mask);
        }
    }

    /// Writes the control block as it now stands.
    pub(crate) fn finish(&self, os: &mut Os<'_>) -> Result<(), Fail> {
        self.sprite.write(os)
    }

    /// Gives the sprite a mask of `bytes` after its image, every bit set.
    pub(crate) fn add_mask(&mut self, os: &mut Os<'_>, bytes: u32) -> Result<(), Fail> {
        let at = bytes32(os, u64::from(self.sprite.image) + self.sprite.image_bytes())?;
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
    let pixels = target.reshapeable(os)?;
    let bytes = bytes32(os, target.sprite.mask_bytes(&pixels))?;
    if target.sprite.masked() {
        return os.fill(target.sprite.address(target.sprite.mask), bytes, 0xFF);
    }
    target.add_mask(os, bytes)?;
    target.finish(os)
}

/// `OS_SpriteOp` 30: the mask removed, if there is one.
pub(crate) fn remove_mask(os: &mut Os<'_>, target: &Target) -> Result<(), Fail> {
    let mut target = *target;
    if !target.sprite.masked() {
        return Ok(());
    }
    let pixels = target.reshapeable(os)?;
    let bytes = bytes32(os, target.sprite.mask_bytes(&pixels))?;
    target.drop_mask(os, bytes)?;
    target.finish(os)
}

/// `OS_SpriteOp` 33: upside down, the mask with it.
pub(crate) fn flip_rows(os: &mut Os<'_>, target: &Target) -> Result<(), Fail> {
    let sprite = &target.sprite;
    let pixels = sprite.pixels(os)?;
    sprite.whole(os, &pixels)?;
    flip_block(
        os,
        sprite.address(sprite.image),
        sprite.rows(),
        sprite.stride(),
    )?;
    if sprite.masked() {
        flip_block(
            os,
            sprite.address(sprite.mask),
            sprite.rows(),
            sprite.mask_stride(&pixels),
        )?;
    }
    Ok(())
}

/// Exchanges the first and last of `rows` rows of `stride` bytes at
/// `address`, and so on inwards.
fn flip_block(os: &mut Os<'_>, address: u32, rows: u32, stride: u64) -> Result<(), Fail> {
    let mut top = row_buffer(os, stride)?;
    let mut bottom = row_buffer(os, stride)?;
    for row in 0..rows / 2 {
        let (upper, lower) = (
            advanced(address, stride * u64::from(row)),
            advanced(address, stride * u64::from(rows - 1 - row)),
        );
        os.read_bytes(upper, &mut top)?;
        os.read_bytes(lower, &mut bottom)?;
        os.write_bytes(upper, &bottom)?;
        os.write_bytes(lower, &top)?;
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
    let pixels = sprite.pixels(os)?;
    sprite.whole(os, &pixels)?;
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
    mirror_block(os, &sprite, sprite.image, sprite.stride(), image)?;
    if sprite.masked() {
        let mask = if pixels.old_format {
            image
        } else {
            Mirror::Bits {
                used: u64::from(sprite.width(&pixels)) * u64::from(pixels.mask_bits()),
            }
        };
        mirror_block(os, &sprite, sprite.mask, sprite.mask_stride(&pixels), mask)?;
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
    /// Their first `used` bits, each bit a field, every bit past them cleared.
    Bits { used: u64 },
}

/// Mirrors every row of the block at the sprite's byte `at`.
fn mirror_block(
    os: &mut Os<'_>,
    sprite: &Sprite,
    at: u32,
    stride: u64,
    mirror: Mirror,
) -> Result<(), Fail> {
    let mut row = row_buffer(os, stride)?;
    let bits_used = match mirror {
        Mirror::Bits { used } => used.min(stride.saturating_mul(8)),
        Mirror::Fields { .. } => 0,
    };
    let mut reversed = row_buffer(os, bits_used.div_ceil(8))?;
    for index in 0..sprite.rows() {
        let address = advanced(sprite.address(at), stride * u64::from(index));
        os.read_bytes(address, &mut row)?;
        match mirror {
            Mirror::Fields {
                count,
                bits,
                clear_rest,
            } => {
                for p in 0..count / 2 {
                    let q = count - 1 - p;
                    let (a, b) = (field(&row, p, bits), field(&row, q, bits));
                    set_field(&mut row, p, bits, b);
                    set_field(&mut row, q, bits, a);
                }
                if clear_rest {
                    clear_from(&mut row, u64::from(count) * u64::from(bits));
                }
            }
            Mirror::Bits { .. } => reverse_bits(&mut row, bits_used, &mut reversed),
        }
        os.write_bytes(address, &row)?;
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

/// The `bits` at bit `bit` of `row`; bits past its end read as nought.
pub(crate) fn field_at(row: &[u8], bit: u64, bits: u32) -> u32 {
    let mut value = 0u64;
    let first = usize::try_from(bit / 8).unwrap_or(usize::MAX);
    for (i, byte) in row.iter().skip(first).take(5).enumerate() {
        value |= u64::from(*byte) << (8 * i);
    }
    let shifted = value >> (bit % 8);
    u32::try_from(shifted & ((1u64 << bits) - 1)).unwrap_or(0)
}

/// Sets pixel `index` of `bits` bits along `row` to `value`.
pub(crate) fn set_field(row: &mut [u8], index: u32, bits: u32, value: u32) {
    set_field_at(row, u64::from(index) * u64::from(bits), bits, value);
}

/// Sets the `bits` at bit `bit` of `row` to `value`; bits past its end are
/// dropped.
pub(crate) fn set_field_at(row: &mut [u8], bit: u64, bits: u32, value: u32) {
    let first = usize::try_from(bit / 8).unwrap_or(usize::MAX);
    let shift = bit % 8;
    let mask = ((1u64 << bits) - 1) << shift;
    let placed = u64::from(value) << shift;
    for (i, byte) in row.iter_mut().skip(first).take(5).enumerate() {
        let lane = 8 * i;
        let m = (mask >> lane) & 0xFF;
        let v = (placed >> lane) & 0xFF;
        *byte = u8::try_from((u64::from(*byte) & !m) | (v & m)).unwrap_or(0);
    }
}

/// `OS_SpriteOp` 54: an old-format sprite's rows moved down to bit nought,
/// a word fewer where its right-hand wastage then reaches one; nothing for a
/// new-format sprite, which has none.
///
/// RISC OS 5 shifts the next word's bits into the wastage on the right; they
/// are nought here, which no plot shows.
pub(crate) fn remove_wastage(os: &mut Os<'_>, target: &Target) -> Result<(), Fail> {
    let mut target = *target;
    let pixels = target.reshapeable(os)?;
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
    let (old, new) = (sprite.stride(), 4 * u64::from(words));
    let moved = move |from: &[u8], to: &mut [u8]| {
        for bit in (0..u64::from(used)).step_by(8) {
            let take = u32::try_from((u64::from(used) - bit).min(8)).unwrap_or(8);
            set_field_at(to, bit, take, field_at(from, bit + u64::from(shift), take));
        }
    };
    let masked = sprite.masked();
    target.restride(os, sprite.image, sprite.rows(), (old, new), moved)?;
    if masked {
        let mask = target.sprite.mask;
        target.restride(os, mask, sprite.rows(), (old, new), moved)?;
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
    let pixels = target.reshapeable(os)?;
    let sprite = target.sprite;
    let rows = sprite.rows();
    if !change.fits(rows) {
        return Err(os.error(OUTSIDE));
    }
    let blocks: [(bool, u64); 2] = [
        (false, sprite.stride()),
        (true, sprite.mask_stride(&pixels)),
    ];
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
    let pixels = target.reshapeable(os)?;
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
    let bits = first + u64::from(new_width) * u64::from(field);
    let words = bytes32(os, bits.div_ceil(32))?;
    let new_stride = 4 * u64::from(words);
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
    let masked = sprite.masked();
    target.restride(
        os,
        sprite.image,
        sprite.rows(),
        (sprite.stride(), new_stride),
        regrouped(first, field),
    )?;
    if masked {
        let (offset, mask_bits) = if pixels.old_format {
            (first, field)
        } else {
            (0, pixels.mask_bits())
        };
        let new_mask_stride = if pixels.old_format {
            new_stride
        } else {
            u64::from(new_width)
                .saturating_mul(u64::from(mask_bits))
                .div_ceil(32)
                * 4
        };
        let mask = target.sprite.mask;
        target.restride(
            os,
            mask,
            sprite.rows(),
            (sprite.mask_stride(&pixels), new_mask_stride),
            regrouped(offset, mask_bits),
        )?;
    }
    target.sprite.words_less_one = words.wrapping_sub(1);
    target.sprite.last_bit = u32::try_from(bits.wrapping_sub(1) % 32).unwrap_or(31);
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
                target.reshapeable(os)?;
                resize_palette(os, &mut target, held, 0)?;
            }
            return Ok(());
        }
        _ => {}
    }
    let pixels = target.reshapeable(os)?;
    if !pixels.paletted() {
        return Err(os.error(NO_DEEP_PALETTE));
    }
    let entries = if pixels.log2_bpp == 3 {
        if regs.r[3] >> 31 != 0 {
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
    for (i, colour) in (0u32..).zip(colours) {
        let at = target
            .sprite
            .address(SPRITE_HEADER_LEN + i * SPRITE_PALETTE_ENTRY_LEN);
        os.write_word(at, colour)?;
        os.write_word(at.wrapping_add(4), colour)?;
    }
    Ok(())
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
    let entry_word = |os: &mut Os<'_>, i: u32| {
        os.read_word(sprite.address(SPRITE_HEADER_LEN + i * SPRITE_PALETTE_ENTRY_LEN))
    };
    if held != 0 && pixels.log2_bpp == 3 {
        let mut bases = [([0u8; 3], 0u8); 16];
        for (k, base) in (0u32..).zip(bases.iter_mut()) {
            let word = if k < held { entry_word(os, k)? } else { 0 };
            let [low, r, g, b] = word.to_le_bytes();
            *base = ([r, g, b].map(|v| (v >> 4) * 0x11), low | low >> 4);
        }
        for i in 0..entries {
            let index = i.to_le_bytes()[0];
            let (rgb, low) = bases[usize::from(index & 0xF)];
            colours.push(rgb_word(grouped(rgb, index)) | u32::from(low));
        }
    } else if held != 0 {
        for i in 0..entries {
            colours.push(entry_word(os, i)? | BOTH_STATES);
        }
    } else {
        for i in 0..entries {
            let index = i.to_le_bytes()[0];
            let rgb = match pixels.log2_bpp {
                0 => physical(DEFAULT_2[usize::from(index & 1)]),
                1 => physical(DEFAULT_4[usize::from(index & 3)]),
                2 => physical(index & 7),
                _ => default_256(index),
            };
            colours.push(rgb_word(rgb));
        }
    }
    Ok(colours)
}

/// A colour as a palette word set for both flash states.
fn rgb_word([r, g, b]: [u8; 3]) -> u32 {
    tairix_colour::Rgb::new(r, g, b).riscos_word() | BOTH_STATES
}

/// One of the eight solid physical colours, bits 0 to 2 red, green and blue.
fn physical(colour: u8) -> [u8; 3] {
    [1, 2, 4].map(|bit| if colour & bit != 0 { 0xFF } else { 0 })
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
