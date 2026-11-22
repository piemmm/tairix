//! A sprite's transparency converted (`OS_SpriteOp` 38) between none, a
//! one-bit mask, an eight-bit alpha mask and an alpha channel in the pixels'
//! top field, which types 5, 6 and 16 alone have room for, as RISC OS 5's
//! SpriteExtend converts it.
//!
//! R3's bits 0 to 7 are the cut-off a value must reach to be opaque in one
//! bit; nought refuses, without error, a conversion that would lose values.
//! Bit 8 removes transparency nothing would be transparent in once converted.
//! The eight-bit mask takes neither. Asked for the form it already has, or a
//! one-bit mask for none, a sprite keeps its transparency. A mask changing
//! form is rewritten where it lies, its pixels' fields alone written; a new
//! mask, and room a mask grows into, begins with every bit set. Whatever
//! happens, the mode word takes its most backward-compatible form, a RISC OS 5
//! word that RISC OS 3.5's form can say becoming one at 90 dots to the inch.

use alloc::vec::Vec;

use tairix_image::{sprite_eig_of_dpi, sprite_mask_stride, sprite_type, SpriteModeWord};
use tairix_riscos_kernel::os::{Fail, Os, Regs};

use super::create::Target;
use super::edit::{bytes32, field_at, row_buffer, set_field_at};
use super::format::{Pixels, BAD_SPRITE_MODE};
use super::header::{advanced, Sprite};
use super::{NO_MEMORY, RESERVED_FLAGS};

/// The forms transparency takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    None,
    Bit,
    Wide,
    Channel,
}

/// The alpha flag of a RISC OS 5 word's mode flags byte.
const ALPHA_FLAG: u8 = 0x80;
/// R3's cut-off and its redundancy bit.
const CUTOFF: u32 = 0xFF;
const DROP_REDUNDANT: u32 = 1 << 8;
/// The resolution RISC OS 5 gives a RISC OS 5 word it rewrites in RISC OS
/// 3.5's form, whatever its eigen factors.
const RO35_DPI: u32 = 90;
/// A value wholly opaque.
const OPAQUE: u8 = 0xFF;

/// What R3 asks.
#[derive(Clone, Copy, Debug)]
struct Request {
    form: Form,
    cutoff: u8,
    drop_redundant: bool,
}

impl Request {
    /// R3's request; none for its reserved operation.
    fn of(r3: u32) -> Option<Self> {
        let cutoff = (r3 & CUTOFF).to_le_bytes()[0];
        let drop_redundant = r3 & DROP_REDUNDANT != 0;
        let (form, cutoff, drop_redundant) = match r3 >> 30 {
            0 => (Form::Bit, cutoff, drop_redundant),
            2 => (Form::Wide, 0, false),
            3 => (Form::Channel, cutoff, drop_redundant),
            _ => return None,
        };
        Some(Self {
            form,
            cutoff,
            drop_redundant,
        })
    }
}

/// What a new-format sprite's mode word says of its pixels.
#[derive(Clone, Copy, Debug)]
struct Word {
    sprite_type: u8,
    flags: u8,
    eig: (u8, u8),
    /// A RISC OS 3.5 word's resolutions, kept while its form is.
    dpi: Option<(u32, u32)>,
}

impl Word {
    fn of(word: u32) -> Option<Self> {
        match SpriteModeWord::of(word) {
            SpriteModeWord::Dpi {
                sprite_type, dpi, ..
            } => {
                let dpi = (u32::from(dpi.0), u32::from(dpi.1));
                Some(Self {
                    sprite_type,
                    flags: 0,
                    eig: (sprite_eig_of_dpi(dpi.0), sprite_eig_of_dpi(dpi.1)),
                    dpi: Some(dpi),
                })
            }
            SpriteModeWord::Eig {
                sprite_type,
                flags,
                eig,
                well_formed: true,
                ..
            } => Some(Self {
                sprite_type,
                flags,
                eig,
                dpi: None,
            }),
            _ => None,
        }
    }

    /// The bits of alpha its pixels' top field holds, where it holds any.
    const fn channel(&self) -> Option<u32> {
        if self.flags & ALPHA_FLAG == 0 {
            None
        } else {
            channel_room(self.sprite_type)
        }
    }

    /// Its most backward-compatible word with transparency `form`.
    fn word(&self, form: Form) -> u32 {
        let wide = form == Form::Wide;
        let flags = if form == Form::Channel {
            self.flags | ALPHA_FLAG
        } else {
            self.flags & !ALPHA_FLAG
        };
        if flags == 0 && self.sprite_type < 16 {
            let dpi = self.dpi.unwrap_or((RO35_DPI, RO35_DPI));
            return SpriteModeWord::dpi_word(self.sprite_type, dpi, wide);
        }
        SpriteModeWord::eig_word(self.sprite_type, flags, self.eig, wide)
    }
}

/// The bits of alpha channel a type has room for: 32K colours' one, 4K's
/// four, 16M's eight.
const fn channel_room(sprite_type: u8) -> Option<u32> {
    match sprite_type {
        sprite_type::DIRECT_15 => Some(1),
        sprite_type::DIRECT_12 => Some(4),
        sprite_type::DIRECT_24 => Some(8),
        _ => None,
    }
}

/// Where a sprite's transparency is, and the bits each value of it takes.
#[derive(Clone, Copy, Debug)]
struct Present {
    form: Form,
    bits: u32,
}

impl Present {
    fn of(sprite: &Sprite, pixels: &Pixels, word: &Word) -> Self {
        if sprite.masked() {
            let form = if pixels.wide_mask {
                Form::Wide
            } else {
                Form::Bit
            };
            return Self {
                form,
                bits: pixels.mask_bits(),
            };
        }
        match word.channel() {
            Some(bits) => Self {
                form: Form::Channel,
                bits,
            },
            None => Self {
                form: Form::None,
                bits: 8,
            },
        }
    }
}

/// `OS_SpriteOp` 38.
pub(crate) fn convert(os: &mut Os<'_>, target: &Target, regs: &mut Regs) -> Result<(), Fail> {
    let asked = regs.r[3];
    regs.r[3] = 0;
    let mut target = *target;
    let pixels = target.sprite.pixels(os)?;
    let Some(word) = Word::of(target.sprite.mode).filter(|_| !pixels.old_format) else {
        return Err(os.error(BAD_SPRITE_MODE));
    };
    let Some(request) = Request::of(asked) else {
        return Err(os.error(RESERVED_FLAGS));
    };
    let bits = match request.form {
        Form::Bit => 1,
        Form::Wide => 8,
        _ => match channel_room(word.sprite_type) {
            Some(bits) => bits,
            None => return Err(os.error(BAD_SPRITE_MODE)),
        },
    };
    target.area.holds(os, &target.sprite)?;
    target.sprite.whole(os, &pixels)?;
    let present = Present::of(&target.sprite, &pixels, &word);
    let kept_as_is =
        present.form == request.form || (present.form == Form::None && request.form == Form::Bit);
    if kept_as_is {
        return rewrite_word(os, &mut target, word.word(present.form));
    }
    let alphas = read_alphas(os, &target.sprite, &pixels, present)?;
    let loses = bits < present.bits && !alphas.iter().all(|&alpha| holds(alpha, bits));
    if request.cutoff == 0 && loses {
        return rewrite_word(os, &mut target, word.word(present.form));
    }
    let threshold = request.cutoff.max(1);
    let reduce = move |alpha: u8| -> u32 {
        match bits {
            1 => u32::from(alpha >= threshold),
            4 => u32::from(alpha >> 4),
            _ => u32::from(alpha),
        }
    };
    let full = (1 << bits) - 1;
    let form = if request.drop_redundant && alphas.iter().all(|&alpha| reduce(alpha) == full) {
        Form::None
    } else {
        request.form
    };
    if form == present.form {
        return rewrite_word(os, &mut target, word.word(present.form));
    }
    let values = Values {
        alphas: &alphas,
        width: target.sprite.width(&pixels),
        reduce: &reduce,
    };
    let conversion = Conversion {
        pixels,
        present,
        form,
        bits,
    };
    conversion.apply(os, &mut target, &values, regs)?;
    target.sprite.mode = word.word(form);
    target.sprite.write(os)
}

/// Whether `alpha` survives being held in `bits` bits.
const fn holds(alpha: u8, bits: u32) -> bool {
    match bits {
        1 => alpha == 0 || alpha == OPAQUE,
        4 => alpha >> 4 == alpha & 0xF,
        _ => true,
    }
}

/// Writes the sprite's mode word alone.
fn rewrite_word(os: &mut Os<'_>, target: &mut Target, word: u32) -> Result<(), Fail> {
    target.sprite.mode = word;
    target.sprite.write(os)
}

/// Every pixel's alpha, rows top first, a pixel without transparency opaque.
fn read_alphas(
    os: &mut Os<'_>,
    sprite: &Sprite,
    pixels: &Pixels,
    present: Present,
) -> Result<Vec<u8>, Fail> {
    let (width, rows) = (sprite.width(pixels), sprite.rows());
    let count =
        usize::try_from(u64::from(width) * u64::from(rows)).map_err(|_| os.error(NO_MEMORY))?;
    let mut alphas = Vec::new();
    if alphas.try_reserve_exact(count).is_err() {
        return Err(os.error(NO_MEMORY));
    }
    let (start, stride, step, offset) = match present.form {
        Form::None => {
            alphas.resize(count, OPAQUE);
            return Ok(alphas);
        }
        Form::Bit | Form::Wide => (sprite.mask, sprite.mask_stride(pixels), present.bits, 0),
        Form::Channel => (
            sprite.image,
            sprite.stride(),
            pixels.field_bits(),
            pixels.field_bits() - present.bits,
        ),
    };
    let mut row = row_buffer(os, stride)?;
    for y in 0..rows {
        os.read_bytes(
            advanced(sprite.address(start), stride * u64::from(y)),
            &mut row,
        )?;
        for x in 0..width {
            let bit = u64::from(x) * u64::from(step) + u64::from(offset);
            alphas.push(widened(field_at(&row, bit, present.bits), present.bits));
        }
    }
    Ok(alphas)
}

/// A value of `bits` bits widened to eight, as an alpha.
fn widened(value: u32, bits: u32) -> u8 {
    let wide = match bits {
        1 => value * u32::from(OPAQUE),
        4 => value * 0x11,
        _ => value,
    };
    wide.to_le_bytes()[0]
}

/// The values a conversion writes: each pixel's alpha, reduced to the
/// destination's bits.
struct Values<'a> {
    alphas: &'a [u8],
    width: u32,
    reduce: &'a dyn Fn(u8) -> u32,
}

impl Values<'_> {
    /// Row `y`'s values, its pixels from the left.
    fn row(&self, y: u32) -> impl Iterator<Item = u32> + '_ {
        let width = self.width as usize;
        let start = (y as usize).saturating_mul(width);
        self.alphas
            .get(start..start.saturating_add(width))
            .unwrap_or_default()
            .iter()
            .map(|&alpha| (self.reduce)(alpha))
    }
}

/// A conversion decided on: the sprite's pixels, where its transparency is
/// and the form and bits it takes.
struct Conversion {
    pixels: Pixels,
    present: Present,
    form: Form,
    bits: u32,
}

impl Conversion {
    /// Makes the transparency, the mask made, remade or removed and the
    /// pixels' top field written or cleared; "Not enough memory", R3 the
    /// bytes the area lacks, where the area cannot hold the result, and
    /// nothing changed.
    fn apply(
        &self,
        os: &mut Os<'_>,
        target: &mut Target,
        values: &Values<'_>,
        regs: &mut Regs,
    ) -> Result<(), Fail> {
        let sprite = target.sprite;
        let rows = u64::from(sprite.rows());
        let old_stride = if sprite.masked() {
            sprite.mask_stride(&self.pixels)
        } else {
            0
        };
        let new_stride = match self.form {
            Form::Bit | Form::Wide => {
                sprite_mask_stride(values.width, self.bits).map_or(u64::MAX, u64::from)
            }
            Form::None | Form::Channel => 0,
        };
        let grows = new_stride.saturating_sub(old_stride).saturating_mul(rows);
        let block = target.area.block(os)?;
        let room = u64::from(block.size.saturating_sub(block.free));
        if grows > room {
            regs.r[3] = u32::try_from(grows - room).unwrap_or(u32::MAX);
            return Err(os.error(NO_MEMORY));
        }
        let old_bytes = bytes32(os, old_stride.saturating_mul(rows))?;
        let new_bytes = bytes32(os, new_stride.saturating_mul(rows))?;
        match (self.present.form, self.form) {
            (Form::Bit | Form::Wide, Form::Bit | Form::Wide) => {
                self.remask(os, target, values, (old_stride, new_stride))
            }
            (_, Form::Bit | Form::Wide) => {
                if self.present.form == Form::Channel {
                    self.write_channel(os, target, None)?;
                }
                target.add_mask(os, new_bytes)?;
                self.write_mask(os, target, values, new_stride)
            }
            (_, Form::Channel) => {
                if sprite.masked() {
                    target.drop_mask(os, old_bytes)?;
                }
                self.write_channel(os, target, Some(values))
            }
            (_, Form::None) => {
                if sprite.masked() {
                    target.drop_mask(os, old_bytes)?;
                }
                Ok(())
            }
        }
    }

    /// Rewrites the mask where it lies as rows of `new` bytes rather than
    /// `old`, room it grows into opened after it.
    fn remask(
        &self,
        os: &mut Os<'_>,
        target: &mut Target,
        values: &Values<'_>,
        (old, new): (u64, u64),
    ) -> Result<(), Fail> {
        let rows = u64::from(target.sprite.rows());
        let at = target.sprite.mask;
        let end = bytes32(os, u64::from(at) + old * rows)?;
        if new > old {
            let grown = bytes32(os, (new - old) * rows)?;
            target.open(os, end, grown, NO_MEMORY)?;
            target.past(end, |offset| offset.wrapping_add(grown));
            os.fill(target.sprite.address(end), grown, 0xFF)?;
        }
        self.write_mask(os, target, values, new)?;
        if new < old {
            let kept = bytes32(os, u64::from(at) + new * rows)?;
            let shrunk = end - kept;
            target.close(os, kept, shrunk)?;
            target.past(end, |offset| offset.wrapping_sub(shrunk));
        }
        Ok(())
    }

    /// Writes each pixel's value into its field of the mask's rows of
    /// `stride` bytes, every other bit as it is.
    fn write_mask(
        &self,
        os: &mut Os<'_>,
        target: &Target,
        values: &Values<'_>,
        stride: u64,
    ) -> Result<(), Fail> {
        let sprite = &target.sprite;
        let mut row = row_buffer(os, stride)?;
        for y in 0..sprite.rows() {
            let at = advanced(sprite.address(sprite.mask), stride * u64::from(y));
            os.read_bytes(at, &mut row)?;
            for (x, value) in (0u64..).zip(values.row(y)) {
                set_field_at(&mut row, x * u64::from(self.bits), self.bits, value);
            }
            os.write_bytes(at, &row)?;
        }
        Ok(())
    }

    /// Writes each pixel's value into its top field, or clears the field
    /// where there are none.
    fn write_channel(
        &self,
        os: &mut Os<'_>,
        target: &Target,
        values: Option<&Values<'_>>,
    ) -> Result<(), Fail> {
        let sprite = &target.sprite;
        let field = self.pixels.field_bits();
        let channel = match values {
            Some(_) => self.bits,
            None => self.present.bits,
        };
        let offset = u64::from(field - channel);
        let stride = sprite.stride();
        let width = sprite.width(&self.pixels);
        let mut row = row_buffer(os, stride)?;
        for y in 0..sprite.rows() {
            let at = advanced(sprite.address(sprite.image), stride * u64::from(y));
            os.read_bytes(at, &mut row)?;
            let mut row_values = values.map(|values| values.row(y));
            for x in 0..u64::from(width) {
                let value = row_values.as_mut().and_then(Iterator::next).unwrap_or(0);
                set_field_at(&mut row, x * u64::from(field) + offset, channel, value);
            }
            os.write_bytes(at, &row)?;
        }
        Ok(())
    }
}
