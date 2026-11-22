//! A sprite as a plot reads it: its image and mask rows, read from guest
//! memory once, its palette, and the transparency its mask or alpha channel
//! gives each pixel.

use alloc::vec::Vec;

use tairix_image::{sprite_type, SpriteModeWord, SPRITE_HEADER_LEN, SPRITE_PALETTE_ENTRY_LEN};
use tairix_riscos_kernel::os::{Fail, Os};

use super::super::edit::field_at;
use super::super::format::{self, Pixels};
use super::super::header::Sprite;
use super::super::NO_MEMORY;
use crate::canvas::mask_between;
use crate::colour::Direct;

/// The alpha flag of a RISC OS 5 word's mode flags byte.
const ALPHA_FLAG: u8 = 0x80;

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
    stride: usize,
    image: Vec<u8>,
    mask_stride: usize,
    mask: Vec<u8>,
}

impl Source {
    /// `sprite` read whole.
    pub(crate) fn read(os: &mut Os<'_>, sprite: &Sprite) -> Result<Self, Fail> {
        let pixels = sprite.pixels(os)?;
        sprite.whole(os, &pixels)?;
        let (width, height) = (sprite.width(&pixels), sprite.rows());
        let channel = channel_bits(sprite.mode, &pixels);
        let transparency = match (sprite.masked(), channel) {
            (true, _) if pixels.wide_mask => Transparency::Wide,
            (true, _) => Transparency::Binary,
            (false, Some(bits)) => Transparency::Channel(bits),
            (false, None) => Transparency::None,
        };
        let stride = bytes(os, sprite.stride())?;
        let image = read_block(os, sprite.address(sprite.image), stride, height)?;
        let (mask_stride, mask) = if sprite.masked() {
            let mask_stride = bytes(os, sprite.mask_stride(&pixels))?;
            (
                mask_stride,
                read_block(os, sprite.address(sprite.mask), mask_stride, height)?,
            )
        } else {
            (0, Vec::new())
        };
        let entries = sprite.named_entries();
        let mut palette = Vec::new();
        if palette.try_reserve_exact(entries as usize).is_err() {
            return Err(os.error(NO_MEMORY));
        }
        for i in 0..entries {
            palette.push(
                os.read_word(sprite.address(SPRITE_HEADER_LEN + i * SPRITE_PALETTE_ENTRY_LEN))?,
            );
        }
        Ok(Self {
            width,
            height,
            pixels,
            first_bit: sprite.first_bit,
            row_bits: u64::from(width) << pixels.log2_field,
            transparency,
            palette,
            direct: direct_of(sprite.mode, &pixels),
            stride,
            image,
            mask_stride,
            mask,
        })
    }

    /// A picture of `width` by `height` opaque pixels, `rgba` four bytes
    /// each from the top left, held as a sprite of 32-bit pixels holds them.
    pub(crate) fn of_rgba(
        os: &mut Os<'_>,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<Self, Fail> {
        let word =
            SpriteModeWord::dpi_word(sprite_type::DIRECT_24, (PICTURE_DPI, PICTURE_DPI), false);
        let pixels = Pixels::stored(word).map_err(|error| os.error(error))?;
        let mut image = Vec::new();
        if image.try_reserve_exact(rgba.len()).is_err() {
            return Err(os.error(NO_MEMORY));
        }
        for &[r, g, b, _] in rgba.as_chunks::<4>().0 {
            image.extend_from_slice(&[r, g, b, 0]);
        }
        Ok(Self {
            width,
            height,
            pixels,
            first_bit: 0,
            row_bits: u64::from(width) << pixels.log2_field,
            transparency: Transparency::None,
            palette: Vec::new(),
            direct: direct_of(word, &pixels),
            stride: bytes(os, u64::from(width) * 4)?,
            image,
            mask_stride: 0,
            mask: Vec::new(),
        })
    }

    /// Row `row`'s bytes, the top row nought.
    pub(crate) fn image_row(&self, row: u32) -> &[u8] {
        let start = row as usize * self.stride;
        self.image
            .get(start..start + self.stride)
            .unwrap_or_default()
    }

    /// Row `row` of the mask, the top row nought.
    pub(crate) fn mask_row(&self, row: u32) -> &[u8] {
        let start = row as usize * self.mask_stride;
        self.mask
            .get(start..start + self.mask_stride)
            .unwrap_or_default()
    }

    /// Pixel `x` of row `row` as stored: its field.
    pub(crate) fn field(&self, x: u32, row: u32) -> u32 {
        let bit = u64::from(self.first_bit) + (u64::from(x) << self.pixels.log2_field);
        field_at(self.image_row(row), bit, self.pixels.field_bits())
    }

    /// Pixel `x` of row `row`'s colour: its field's low bits where a
    /// numbered mode doubles its pixels.
    pub(crate) fn colour(&self, x: u32, row: u32) -> u32 {
        let field = self.field(x, row);
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
            Transparency::Channel(bits) => {
                let field = self.pixels.field_bits();
                let value = (self.field(x, row) >> (field - bits)) & ((1 << bits) - 1);
                match bits {
                    1 => {
                        if value != 0 {
                            0xFF
                        } else {
                            0
                        }
                    }
                    4 => (value * 0x11).to_le_bytes()[0],
                    _ => value.to_le_bytes()[0],
                }
            }
        }
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

/// The resolution a decoded picture is held at: the standard screen's.
const PICTURE_DPI: u32 = 90;

/// How the pixels of a sprite of mode `word` hold their colours.
fn direct_of(word: u32, pixels: &Pixels) -> Direct {
    let flags = format::variable(word, tairix_vdu::mode::var::MODE_FLAGS).unwrap_or(0);
    let ncolour = format::variable(word, tairix_vdu::mode::var::N_COLOUR).unwrap_or(0);
    Direct::of_parts(pixels.log2_bpp, ncolour, flags)
}

/// The bits of alpha channel a sprite's pixels hold, if any.
fn channel_bits(word: u32, pixels: &Pixels) -> Option<u32> {
    let SpriteModeWord::Eig {
        sprite_type, flags, ..
    } = SpriteModeWord::of(word)
    else {
        return None;
    };
    if flags & ALPHA_FLAG == 0 || pixels.old_format {
        return None;
    }
    match sprite_type {
        tairix_image::sprite_type::DIRECT_15 => Some(1),
        tairix_image::sprite_type::DIRECT_12 => Some(4),
        tairix_image::sprite_type::DIRECT_24 => Some(8),
        _ => None,
    }
}

/// A row's bytes as a length; one no sprite could hold is no memory.
fn bytes(os: &mut Os<'_>, stride: u64) -> Result<usize, Fail> {
    usize::try_from(stride).map_err(|_| os.error(NO_MEMORY))
}

/// `rows` rows of `stride` bytes from `address`.
fn read_block(os: &mut Os<'_>, address: u32, stride: usize, rows: u32) -> Result<Vec<u8>, Fail> {
    let length = stride
        .checked_mul(rows as usize)
        .ok_or_else(|| os.error(NO_MEMORY))?;
    os.read_vec(address, length)
}
