//! RISC OS 5.30's reading of a sprite mode word, each rule as recorded: the
//! pixels a sprite of the word holds, what `OS_ReadModeVariable` answers for
//! it, and which words making a sprite accepts. The word's fields are
//! `lib/image`'s one reading of its form.

use tairix_image::{sprite_dpi_of_eig, sprite_type, SpriteModeWord};
use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::os::{Fail, Os};
use tairix_vdu::mode::var;

use crate::mode::Format;
use crate::select::{self, Outcome, Spec};

/// A sprite mode naming no mode a sprite may be in: a text mode, a number with
/// the shadow bit, a word whose form holds no pixels.
pub(crate) const BAD_SPRITE_MODE: Error = Error {
    number: 0x708,
    token: "BadSpriteMode",
};
/// A mode word naming a type or resolution RISC OS does not take.
pub(crate) const BAD_MODE_WORD: Error = Error {
    number: 0x719,
    token: "BadSpriteModeWord",
};
/// A numbered mode neither RISC OS nor any module knows.
pub(crate) const UNKNOWN_MODE: Error = Error {
    number: 0x19,
    token: "UnknownSpriteMode",
};

/// The flag bits of a RISC OS 5 word's eight that RISC OS 5 reads: the red and
/// blue channels exchanged, and alpha in the top field.
const RGB_AND_ALPHA: u8 = 0xC0;
/// The colour space field of those eight: RGB where clear.
const COLOUR_SPACE: u8 = 0x30;
/// The 5:6:5 mode flag type 10 brings.
const FIVE_SIX_FIVE: u32 = 0x80;

/// How a sprite holds its pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pixels {
    /// The bits a pixel's colour takes, as a power of two.
    pub(crate) log2_bpp: u32,
    /// The bits a pixel takes of a row, as a power of two: more only in a
    /// numbered mode that doubles its pixels on the screen.
    pub(crate) log2_field: u32,
    /// A numbered mode's: its mask has the image's form, and its rows may
    /// begin part way into their first word.
    pub(crate) old_format: bool,
    /// Its mask is eight bits a pixel of alpha.
    pub(crate) wide_mask: bool,
}

impl Pixels {
    /// A numbered mode's.
    pub(crate) const fn numbered(format: &Format) -> Self {
        Self {
            log2_bpp: format.log2_bpp(),
            log2_field: format.log2_bpc(),
            old_format: true,
            wide_mask: false,
        }
    }

    /// The pixels a stored sprite's word gives, as reading a sprite takes
    /// them: a type RISC OS does not support holds thirty-two bits.
    ///
    /// # Errors
    ///
    /// A numbered mode no sprite may be in, a mode selector's address, and a
    /// RISC OS 5 word naming a colour space other than RGB.
    pub(crate) fn stored(word: u32) -> Result<Self, Error> {
        match reading(word) {
            SpriteModeWord::Number(number) => tairix_vdu::mode::numbered(u32::from(number))
                .filter(|mode| mode.graphic())
                .map(|mode| Self::numbered(&Format::numbered(mode)))
                .ok_or(BAD_SPRITE_MODE),
            SpriteModeWord::Selector => Err(BAD_SPRITE_MODE),
            SpriteModeWord::Dpi {
                sprite_type,
                wide_mask,
                ..
            } => Ok(Self::typed(sprite_type, wide_mask)),
            SpriteModeWord::Eig {
                sprite_type,
                flags,
                wide_mask,
                ..
            } => {
                if flags & COLOUR_SPACE != 0 {
                    return Err(BAD_MODE_WORD);
                }
                Ok(Self::typed(sprite_type, wide_mask))
            }
        }
    }

    /// The pixels a word gives as verifying an area reads them: any number a
    /// numbered mode's form, any type its own whatever the word's flags.
    pub(crate) fn verified(word: u32) -> Self {
        match reading(word) {
            SpriteModeWord::Number(_) | SpriteModeWord::Selector => {
                Self::stored(word).unwrap_or(Self {
                    log2_bpp: 0,
                    log2_field: 0,
                    old_format: true,
                    wide_mask: false,
                })
            }
            SpriteModeWord::Dpi {
                sprite_type,
                wide_mask,
                ..
            }
            | SpriteModeWord::Eig {
                sprite_type,
                wide_mask,
                ..
            } => Self::typed(sprite_type, wide_mask),
        }
    }

    /// A new-format sprite's of `sprite_type`.
    const fn typed(sprite_type: u8, wide_mask: bool) -> Self {
        let log2_bpp = type_reading(sprite_type).log2_bpp;
        Self {
            log2_bpp,
            log2_field: log2_bpp,
            old_format: false,
            wide_mask,
        }
    }

    /// The bits a pixel takes of a row.
    pub(crate) const fn field_bits(&self) -> u32 {
        1 << self.log2_field
    }

    /// The bits a pixel's colour takes.
    pub(crate) const fn bits(&self) -> u32 {
        1 << self.log2_bpp
    }

    /// Whether its pixels are colour numbers through a palette.
    pub(crate) const fn paletted(&self) -> bool {
        self.log2_bpp <= 3
    }

    /// The bits a pixel of its mask takes.
    pub(crate) const fn mask_bits(&self) -> u32 {
        if self.old_format {
            self.field_bits()
        } else if self.wide_mask {
            8
        } else {
            1
        }
    }
}

/// The word as RISC OS 5 reads it: a RISC OS 5 word lacking its fixed pattern
/// is taken in RISC OS 3.5's form.
fn reading(word: u32) -> SpriteModeWord {
    match SpriteModeWord::of(word) {
        SpriteModeWord::Eig {
            well_formed: false, ..
        } => SpriteModeWord::dpi_form(word),
        form => form,
    }
}

/// What RISC OS 5.30 makes of a sprite type.
#[derive(Clone, Copy)]
struct TypeReading {
    log2_bpp: u32,
    ncolour: u32,
    flags: u32,
}

/// A sprite type's reading; a type RISC OS 5.30 does not support reads as
/// type 6.
const fn type_reading(sprite_type: u8) -> TypeReading {
    let (log2_bpp, ncolour, flags) = match sprite_type {
        sprite_type::PALETTED_1 => (0, 1, 0),
        sprite_type::PALETTED_2 => (1, 3, 0),
        sprite_type::PALETTED_4 => (2, 15, 0),
        sprite_type::PALETTED_8 => (3, 255, 0),
        sprite_type::DIRECT_15 => (4, 0xFFFF, 0),
        sprite_type::DIRECT_16 => (4, 0xFFFF, FIVE_SIX_FIVE),
        sprite_type::DIRECT_12 => (4, 0xFFF, 0),
        _ => (5, u32::MAX, 0),
    };
    TypeReading {
        log2_bpp,
        ncolour,
        flags,
    }
}

/// The highest RISC OS 5 type `OS_ReadModeVariable` answers for.
const LAST_READ_TYPE: u8 = sprite_type::YCBCR_422;

/// The eigen factor a resolution names: none for one no eigen factor gives.
const fn eig_of_dpi(dpi: u16) -> Option<u32> {
    match dpi {
        180 => Some(0),
        90 => Some(1),
        45 => Some(2),
        22 | 23 => Some(3),
        _ => None,
    }
}

/// Variable `number` of sprite mode word `word`, as `OS_ReadModeVariable`
/// reads it; none where RISC OS 5.30 answers with C set. `word` is a word
/// rather than a number or a selector's address.
pub(crate) fn variable(word: u32, number: u32) -> Option<u32> {
    let (sprite_type, flags, eig) = match reading(word) {
        SpriteModeWord::Dpi {
            sprite_type, dpi, ..
        } => {
            let reading = type_reading(sprite_type);
            (
                reading,
                Some(reading.flags),
                (eig_of_dpi(dpi.0), eig_of_dpi(dpi.1)),
            )
        }
        SpriteModeWord::Eig {
            sprite_type,
            flags,
            eig,
            ..
        } => {
            if sprite_type == 0 || sprite_type > LAST_READ_TYPE {
                return None;
            }
            let reading = type_reading(sprite_type);
            let readable = flags & !RGB_AND_ALPHA == 0;
            (
                reading,
                readable.then_some(u32::from(flags) << 8 | reading.flags),
                (Some(u32::from(eig.0)), Some(u32::from(eig.1))),
            )
        }
        SpriteModeWord::Number(_) | SpriteModeWord::Selector => return None,
    };
    match number {
        var::MODE_FLAGS => flags,
        var::N_COLOUR => Some(sprite_type.ncolour),
        var::X_EIG => eig.0,
        var::Y_EIG => eig.1,
        var::Y_SHIFT => Some(0),
        var::LOG2_BPP | var::LOG2_BPC => Some(sprite_type.log2_bpp),
        _ => None,
    }
}

/// The pixels and stored word of a sprite made in mode `r6`, a numbered mode,
/// a sprite mode word, or a mode selector's address, which becomes the most
/// backward-compatible word naming the same pixels.
///
/// # Errors
///
/// RISC OS 5.30's refusal of the mode, or a fault reading a selector.
pub(crate) fn for_new_sprite(os: &mut Os<'_>, r6: u32) -> Result<(Pixels, u32), Fail> {
    match reading(r6) {
        SpriteModeWord::Number(number) => {
            let format = numbered_format(os, number)?;
            Ok((Pixels::numbered(&format), r6))
        }
        SpriteModeWord::Selector => {
            let format = match select::read_spec(os, r6)? {
                Some(Spec::Selector(selector)) => selector.format(),
                _ => None,
            };
            let word = format
                .as_ref()
                .and_then(word_for)
                .ok_or_else(|| os.error(BAD_SPRITE_MODE))?;
            Ok((Pixels::stored(word).map_err(|e| os.error(e))?, word))
        }
        SpriteModeWord::Dpi {
            sprite_type,
            dpi,
            wide_mask,
        } => {
            let named =
                sprite_type != 0 && eig_of_dpi(dpi.0).is_some() && eig_of_dpi(dpi.1).is_some();
            if !named {
                return Err(os.error(BAD_MODE_WORD));
            }
            Ok((Pixels::typed(sprite_type, wide_mask), r6))
        }
        SpriteModeWord::Eig {
            sprite_type,
            flags,
            wide_mask,
            ..
        } => {
            if sprite_type == 0 || !flags_allowed(sprite_type, flags) {
                return Err(os.error(BAD_MODE_WORD));
            }
            Ok((Pixels::typed(sprite_type, wide_mask), r6))
        }
    }
}

/// Whether a RISC OS 5 word of `sprite_type` may carry `flags`: a paletted
/// type none, any other the channels exchanged and alpha.
const fn flags_allowed(sprite_type: u8, flags: u8) -> bool {
    if type_reading(sprite_type).log2_bpp <= 3 {
        flags == 0
    } else {
        flags & !RGB_AND_ALPHA == 0
    }
}

/// Numbered mode `number`'s format: RISC OS 5's own, or one a module
/// describes for a number it does not know.
fn numbered_format(os: &mut Os<'_>, number: u8) -> Result<Format, Fail> {
    if let Some(mode) = tairix_vdu::mode::numbered(u32::from(number & 0x7F)) {
        if !mode.graphic() || number >= 0x80 {
            return Err(os.error(BAD_SPRITE_MODE));
        }
        return Ok(Format::numbered(mode));
    }
    let monitor = crate::state::with(os, |vdu, _| Ok(vdu.monitor))?;
    match select::resolve(os, &Spec::Number(u32::from(number)), monitor)? {
        Outcome::Valid(resolved) if number < 0x80 && resolved.format.graphic() => {
            Ok(resolved.format)
        }
        _ => Err(os.error(UNKNOWN_MODE)),
    }
}

/// The most backward-compatible sprite mode word for a screen of `format`:
/// RISC OS 3.5's form wherever it can name the pixels, else RISC OS 5's; none
/// for pixels neither names.
pub(crate) fn word_for(format: &Format) -> Option<u32> {
    let flags = format.flags();
    let high = (flags >> 8).to_le_bytes()[0];
    let sprite_type = match (format.log2_bpp(), format.ncolour()) {
        (0, _) => sprite_type::PALETTED_1,
        (1, _) => sprite_type::PALETTED_2,
        (2, _) => sprite_type::PALETTED_4,
        (3, _) => sprite_type::PALETTED_8,
        (4, 0xFFF) => sprite_type::DIRECT_12,
        (4, _) if flags & FIVE_SIX_FIVE != 0 => sprite_type::DIRECT_16,
        (4, _) => sprite_type::DIRECT_15,
        (5, _) => sprite_type::DIRECT_24,
        _ => return None,
    };
    let (x_eig, y_eig) = (format.x_eig(), format.y_eig());
    let eig_fits = x_eig <= 3 && y_eig <= 3;
    let paletted = format.log2_bpp() <= 3;
    if (paletted && high != 0) || !eig_fits || high & !RGB_AND_ALPHA != 0 {
        return None;
    }
    let eig = (x_eig.to_le_bytes()[0], y_eig.to_le_bytes()[0]);
    if high == 0 && sprite_type != sprite_type::DIRECT_12 {
        let dpi = (sprite_dpi_of_eig(eig.0), sprite_dpi_of_eig(eig.1));
        return Some(SpriteModeWord::dpi_word(sprite_type, dpi, false));
    }
    Some(SpriteModeWord::eig_word(sprite_type, high, eig, false))
}

/// The numbered mode or mode word a sprite taken from the screen is in: a
/// numbered screen mode's number, else the word naming the screen's pixels.
pub(crate) fn of_screen(named: crate::screen::Named, format: &Format) -> Option<u32> {
    match named {
        crate::screen::Named::Number(n) => Some(n),
        crate::screen::Named::Selector(_) => word_for(format),
    }
}

/// Whether a stored word's form is one `OS_SpriteOp` 17 passes: a RISC OS 5
/// word with its fixed pattern, or any other.
pub(crate) const fn verifiable(word: u32) -> bool {
    !matches!(
        SpriteModeWord::of(word),
        SpriteModeWord::Eig {
            well_formed: false,
            ..
        }
    )
}

#[cfg(test)]
mod tests;
