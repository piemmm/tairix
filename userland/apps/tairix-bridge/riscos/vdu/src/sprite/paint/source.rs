//! A sprite as a plot reads it: its image and mask rows, read from guest
//! memory once, its palette, and the transparency its mask or alpha channel
//! gives each pixel.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_image::{
    sprite_type, SpriteModeWord, SPRITE_HEADER_LEN, SPRITE_PALETTE_ENTRY_LEN, STANDARD_SPRITE_DPI,
};
use tairix_riscos_kernel::os::{Fail, Os};
use tairix_util::bitfield::field_at;

use super::super::alpha::{channel_room, widened, ALPHA_FLAG};
use super::super::format::{self, Pixels};
use super::super::header::{Extents, Sprite};
use super::super::NO_MEMORY;
use crate::canvas::mask_between;
use crate::colour::Direct;

/// Where a sprite's transparency is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Transparency {
    /// None: every pixel solid.
    None,
    /// A mask of one bit a pixel, or an old-format mask of the image's form.
    Binary,
    /// A mask of eight bits of alpha a pixel.
    Wide,
    /// Alpha in the top `bits` of each pixel.
    Channel(u32),
}

/// A sprite checked whole for a plot, its rows not yet read: what the plot
/// needs to know which of them it shows.
pub(crate) struct Checked<'a> {
    sprite: &'a Sprite,
    extents: Extents,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixels: Pixels,
    pub(crate) transparency: Transparency,
}

impl Checked<'_> {
    /// The bits its rows' pixels take.
    pub(crate) const fn row_bits(&self) -> u64 {
        (self.width as u64) << self.pixels.log2_field
    }

    /// The words each of its rows is held in.
    pub(crate) const fn row_words(&self) -> u32 {
        self.sprite.words_less_one.wrapping_add(1)
    }

    /// The sprite with its rows `rows`, from the top, read, its palette not:
    /// those rows the plot shows.
    pub(crate) fn read(&self, os: &mut Os<'_>, rows: Range<u32>) -> Result<Source, Fail> {
        let sprite = self.sprite;
        let mut source = Source {
            width: self.width,
            height: self.height,
            pixels: self.pixels,
            first_bit: sprite.first_bit,
            row_bits: self.row_bits(),
            transparency: self.transparency,
            palette: Vec::new(),
            direct: direct_of(&self.pixels),
            rows: 0..0,
            stride: bytes(os, self.extents.stride)?,
            image: Vec::new(),
            mask_stride: if sprite.masked() {
                bytes(os, self.extents.mask_stride)?
            } else {
                0
            },
            mask: Vec::new(),
        };
        self.read_rows(os, &mut source, rows)?;
        Ok(source)
    }

    /// As [`Checked::read`], its palette read too: for a plot turning its
    /// colours into the destination's.
    pub(crate) fn coloured(&self, os: &mut Os<'_>, rows: Range<u32>) -> Result<Source, Fail> {
        let mut source = self.read(os, rows)?;
        source.palette = palette(os, self.sprite, self.sprite.entries_named_by(&self.pixels))?;
        Ok(source)
    }

    /// Rows `rows`, from the top, read into `source`, which this sprite's
    /// reading made, in place of those it held.
    pub(crate) fn read_rows(
        &self,
        os: &mut Os<'_>,
        source: &mut Source,
        rows: Range<u32>,
    ) -> Result<(), Fail> {
        let sprite = self.sprite;
        let start = rows.start.min(self.height);
        let rows = start..rows.end.clamp(start, self.height);
        let image = read_block(os, sprite.address(sprite.image), source.stride, &rows)?;
        let mask = if sprite.masked() {
            read_block(os, sprite.address(sprite.mask), source.mask_stride, &rows)?
        } else {
            Vec::new()
        };
        (source.image, source.mask, source.rows) = (image, mask, rows);
        Ok(())
    }
}

/// A sprite read for plotting.
pub(crate) struct Source {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixels: Pixels,
    pub(crate) first_bit: u32,
    /// The bits its rows' pixels take.
    pub(crate) row_bits: u64,
    pub(crate) transparency: Transparency,
    /// Each palette entry's first word, none without a palette.
    pub(crate) palette: Vec<u32>,
    /// How a deep sprite's pixels hold their colours.
    pub(crate) direct: Direct,
    /// The rows read, from the top.
    rows: Range<u32>,
    stride: usize,
    image: Vec<u8>,
    mask_stride: usize,
    mask: Vec<u8>,
}

impl Source {
    /// `sprite` checked whole, its rows yet to be read.
    pub(crate) fn check<'a>(os: &mut Os<'_>, sprite: &'a Sprite) -> Result<Checked<'a>, Fail> {
        let pixels = sprite.pixels(os)?;
        let extents = sprite.whole(os, &pixels)?;
        let transparency = match (sprite.masked(), channel_bits(sprite.mode, &pixels)) {
            (true, _) if pixels.wide_mask => Transparency::Wide,
            (true, _) => Transparency::Binary,
            (false, Some(bits)) => Transparency::Channel(bits),
            (false, None) => Transparency::None,
        };
        Ok(Checked {
            sprite,
            extents,
            width: sprite.width(&pixels),
            height: sprite.rows(),
            pixels,
            transparency,
        })
    }

    /// `sprite` read whole, its palette with it.
    pub(crate) fn read(os: &mut Os<'_>, sprite: &Sprite) -> Result<Self, Fail> {
        let checked = Self::check(os, sprite)?;
        checked.coloured(os, 0..checked.height)
    }

    /// A picture of `width` by `height` opaque pixels, `rgba` four bytes
    /// each from the top left, held as a sprite of 32-bit pixels holds them:
    /// the fourth byte is such a pixel's spare one, outside its colour's bits.
    pub(crate) fn of_rgba(
        os: &mut Os<'_>,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    ) -> Result<Self, Fail> {
        let word = SpriteModeWord::dpi_word(
            sprite_type::DIRECT_24,
            (STANDARD_SPRITE_DPI, STANDARD_SPRITE_DPI),
            false,
        );
        let pixels = Pixels::stored(word).map_err(|error| os.error(error))?;
        let image = rgba;
        Ok(Self {
            width,
            height,
            pixels,
            first_bit: 0,
            row_bits: u64::from(width) << pixels.log2_field,
            transparency: Transparency::None,
            palette: Vec::new(),
            direct: direct_of(&pixels),
            rows: 0..height,
            stride: bytes(os, u64::from(width) * 4)?,
            image,
            mask_stride: 0,
            mask: Vec::new(),
        })
    }

    /// The rows read, from the top.
    pub(crate) fn rows(&self) -> Range<u32> {
        self.rows.clone()
    }

    /// Row `row`'s bytes, the top row nought; none for a row not read.
    pub(crate) fn image_row(&self, row: u32) -> &[u8] {
        held(&self.image, self.stride, row, &self.rows)
    }

    /// Row `row` of the mask, the top row nought; none for a row not read.
    pub(crate) fn mask_row(&self, row: u32) -> &[u8] {
        held(&self.mask, self.mask_stride, row, &self.rows)
    }

    /// Pixel `x` of row `row` as stored: its field.
    pub(crate) fn field(&self, x: u32, row: u32) -> u32 {
        let bit = u64::from(self.first_bit) + (u64::from(x) << self.pixels.log2_field);
        field_at(self.image_row(row), bit, self.pixels.field_bits())
    }

    /// Pixel `x` of row `row`'s colour: its field's low bits where a
    /// numbered mode doubles its pixels.
    pub(crate) fn colour(&self, x: u32, row: u32) -> u32 {
        self.colour_of(self.field(x, row))
    }

    /// The colour a pixel of field `field` holds.
    const fn colour_of(&self, field: u32) -> u32 {
        if self.pixels.log2_bpp >= 5 {
            field
        } else {
            field & ((1 << self.pixels.bits()) - 1)
        }
    }

    /// Pixel `x` of row `row`'s alpha, 255 solid: its mask's or its alpha
    /// channel's.
    pub(crate) fn alpha(&self, x: u32, row: u32) -> u8 {
        match self.transparency {
            Transparency::None => 0xFF,
            Transparency::Binary => {
                if self.solid(x, row) {
                    0xFF
                } else {
                    0
                }
            }
            Transparency::Wide => {
                field_at(self.mask_row(row), u64::from(x) * 8, 8).to_le_bytes()[0]
            }
            Transparency::Channel(bits) => self.channel_alpha(self.field(x, row), bits),
        }
    }

    /// Pixel `x` of row `row`'s alpha where `masked`, else 255, and its
    /// colour; none where its alpha is nought. An alpha channel's field is
    /// read once for both.
    pub(crate) fn pixel(&self, x: u32, row: u32, masked: bool) -> Option<(u8, u32)> {
        let (alpha, field) = match self.transparency {
            Transparency::Channel(bits) if masked => {
                let field = self.field(x, row);
                (self.channel_alpha(field, bits), Some(field))
            }
            _ if masked => (self.alpha(x, row), None),
            _ => (0xFF, None),
        };
        (alpha != 0).then(|| {
            let field = field.unwrap_or_else(|| self.field(x, row));
            (alpha, self.colour_of(field))
        })
    }

    /// The alpha the top `bits` of field `field` hold, 255 solid.
    fn channel_alpha(&self, field: u32, bits: u32) -> u8 {
        let below = self.pixels.field_bits() - bits;
        widened((field >> below) & ((1 << bits) - 1), bits)
    }

    /// Whether pixel `x` of row `row` is solid in a binary mask: an
    /// old-format mask pixel solid where any of its bits is.
    pub(crate) fn solid(&self, x: u32, row: u32) -> bool {
        let bits = self.pixels.mask_bits();
        let offset = if self.pixels.old_format {
            u64::from(self.first_bit)
        } else {
            0
        };
        field_at(
            self.mask_row(row),
            offset + u64::from(x) * u64::from(bits),
            bits,
        ) != 0
    }

    /// The `bits` image bits from bit `start` of row `row` that are solid,
    /// as the kernel's plots take a mask: an old-format mask's own bits, else
    /// each mask pixel covering every bit of its image pixel. All are solid
    /// without a mask, and with an alpha channel, which the mask flag leaves.
    pub(crate) fn solid_bits(&self, start: u64, bits: u32, row: u32) -> u32 {
        match self.transparency {
            Transparency::None | Transparency::Channel(_) => mask_between(0, bits),
            Transparency::Binary | Transparency::Wide if self.pixels.old_format => {
                field_at(self.mask_row(row), start, bits)
            }
            Transparency::Binary | Transparency::Wide => {
                let (first, log2_field) = (u64::from(self.first_bit), self.pixels.log2_field);
                let mask_bits = self.pixels.mask_bits();
                let end = start + u64::from(bits);
                let mut solid = 0;
                let mut at = start;
                while at < end {
                    let pixel = (at - first) >> log2_field;
                    let next = (first + ((pixel + 1) << log2_field)).min(end);
                    let value =
                        field_at(self.mask_row(row), pixel * u64::from(mask_bits), mask_bits);
                    if value != 0 {
                        let low = u32::try_from(at - start).unwrap_or(0);
                        let high = u32::try_from(next - start).unwrap_or(bits);
                        solid |= mask_between(low, high);
                    }
                    at = next;
                }
                solid
            }
        }
    }
}

/// How `pixels` hold their colours.
const fn direct_of(pixels: &Pixels) -> Direct {
    Direct::of_parts(pixels.log2_bpp, pixels.shown.ncolour, pixels.shown.flags)
}

/// The bits of alpha channel a sprite's pixels hold, if any.
fn channel_bits(word: u32, pixels: &Pixels) -> Option<u32> {
    let SpriteModeWord::Eig {
        sprite_type, flags, ..
    } = format::reading(word)
    else {
        return None;
    };
    if flags & ALPHA_FLAG == 0 || pixels.old_format {
        return None;
    }
    channel_room(sprite_type)
}

/// A row's bytes as a length; one no sprite could hold is no memory.
fn bytes(os: &mut Os<'_>, stride: u64) -> Result<usize, Fail> {
    usize::try_from(stride).map_err(|_| os.error(NO_MEMORY))
}

/// Rows `rows` of the block of rows of `stride` bytes at `address`.
fn read_block(
    os: &mut Os<'_>,
    address: u32,
    stride: usize,
    rows: &Range<u32>,
) -> Result<Vec<u8>, Fail> {
    let offset = |row: u32| usize::try_from(row).ok()?.checked_mul(stride);
    let (Some(start), Some(end)) = (offset(rows.start), offset(rows.end)) else {
        return Err(os.error(NO_MEMORY));
    };
    let Ok(skipped) = u32::try_from(start) else {
        return Err(os.error(NO_MEMORY));
    };
    os.read_vec(address.wrapping_add(skipped), end - start)
}

/// Row `row` of `block`, its rows `rows` of `stride` bytes each; none for a
/// row outside them.
fn held<'a>(block: &'a [u8], stride: usize, row: u32, rows: &Range<u32>) -> &'a [u8] {
    row.checked_sub(rows.start)
        .and_then(|held| usize::try_from(held).ok()?.checked_mul(stride))
        .and_then(|start| block.get(start..start.checked_add(stride)?))
        .unwrap_or_default()
}

/// The first word of each of `sprite`'s first `entries` palette entries.
fn palette(os: &mut Os<'_>, sprite: &Sprite, entries: u32) -> Result<Vec<u32>, Fail> {
    let entries = entries as usize;
    let block = os.read_vec(
        sprite.address(SPRITE_HEADER_LEN),
        entries * SPRITE_PALETTE_ENTRY_LEN as usize,
    )?;
    let mut palette = Vec::new();
    if palette.try_reserve_exact(entries).is_err() {
        return Err(os.error(NO_MEMORY));
    }
    let (entries, _) = block.as_chunks::<{ SPRITE_PALETTE_ENTRY_LEN as usize }>();
    palette.extend(
        entries
            .iter()
            .map(|&[a, b, c, d, ..]| u32::from_le_bytes([a, b, c, d])),
    );
    Ok(palette)
}
