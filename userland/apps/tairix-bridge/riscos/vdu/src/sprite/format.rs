//! RISC OS 5.30's reading of a sprite mode word, each rule as recorded: the
//! pixels a sprite of the word holds, what `OS_ReadModeVariable` answers for
//! it, and which words making a sprite accepts. The word's fields are
//! `lib/image`'s one reading of its form.

use tairix_image::{
    sprite_dpi_of_eig, sprite_eig_of_dpi, sprite_type, SpriteMaskDepth, SpriteMode, SpriteModeWord,
};
use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::os::{Fail, Os};
use tairix_vdu::mode::{var, Flags};

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

/// The flag bits of a RISC OS 5 word's eight, the mode flags' second byte,
/// that RISC OS 5 reads: the red and blue channels exchanged, and alpha in
/// the top field.
const RGB_AND_ALPHA: u8 = (Flags::RGB.bits() | Flags::ALPHA.bits()).to_le_bytes()[1];
/// The colour space field of those eight: RGB where clear.
const COLOUR_SPACE: u8 = 0x30;
/// The 5:6:5 mode flag type 10 brings: in a sixteen-bit mode, the flag a full
/// palette sets in an eight-bit one.
const FIVE_SIX_FIVE: u32 = Flags::FULL_PALETTE.bits();

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
    /// How its pixels show.
    pub(crate) shown: Shown,
}

/// How a sprite's pixels show, as a screen of them would: its eigen factors,
/// and the mode flags, colours and line shift a screen of them reads, a
/// numbered mode's its own, whatever describes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Shown {
    pub(crate) eig: (u32, u32),
    pub(crate) flags: u32,
    pub(crate) ncolour: u32,
    pub(crate) y_shift: u32,
}

impl Pixels {
    /// A numbered mode's.
    pub(crate) fn numbered(format: &Format) -> Self {
        Self {
            log2_bpp: format.log2_bpp(),
            log2_field: format.log2_bpc(),
            old_format: true,
            wide_mask: false,
            shown: Shown {
                eig: (format.x_eig(), format.y_eig()),
                flags: format.flags(),
                ncolour: format.ncolour(),
                y_shift: format.var(var::Y_SHIFT).unwrap_or(0),
            },
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
            } => Ok(Self::typed(word, sprite_type, wide_mask)),
            SpriteModeWord::Eig {
                sprite_type,
                flags,
                wide_mask,
                ..
            } => {
                if flags & COLOUR_SPACE != 0 {
                    return Err(BAD_MODE_WORD);
                }
                Ok(Self::typed(word, sprite_type, wide_mask))
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
                    shown: Shown {
                        eig: (1, 1),
                        flags: 0,
                        ncolour: crate::mode::default_ncolour(0),
                        y_shift: 0,
                    },
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
            } => Self::typed(word, sprite_type, wide_mask),
        }
    }

    /// A new-format sprite's of `sprite_type`, its mode word `word`.
    fn typed(word: u32, sprite_type: u8, wide_mask: bool) -> Self {
        let log2_bpp = type_reading(sprite_type).log2_bpp;
        let (x_eig, y_eig) = drawn_eig(word);
        Self {
            log2_bpp,
            log2_field: log2_bpp,
            old_format: false,
            wide_mask,
            shown: Shown {
                eig: (u32::from(x_eig), u32::from(y_eig)),
                flags: variable(word, var::MODE_FLAGS).unwrap_or(0),
                ncolour: if log2_bpp <= 3 {
                    crate::mode::default_ncolour(log2_bpp)
                } else {
                    variable(word, var::N_COLOUR).unwrap_or(u32::MAX)
                },
                y_shift: 0,
            },
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

    /// The colours its pixels name through a palette; none in direct colour.
    pub(crate) const fn colours(&self) -> u32 {
        if self.paletted() {
            1 << self.bits()
        } else {
            0
        }
    }

    /// The bits a pixel of its mask takes.
    pub(crate) const fn mask_bits(&self) -> u32 {
        SpriteMaskDepth::of(self.old_format, self.wide_mask).bits(self.field_bits())
    }
}

/// The word as RISC OS 5 reads it: a RISC OS 5 word lacking its fixed pattern
/// is taken in RISC OS 3.5's form.
pub(crate) fn reading(word: u32) -> SpriteModeWord {
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

/// The largest eigen factor a mode variable read of a resolution answers.
const MOST_READ_EIG: u8 = crate::mode::MOST_EIG.to_le_bytes()[0];

/// The eigen factor a resolution names: none for one no eigen factor gives.
/// The coarsest's 22.5 dots an inch is written either way.
const fn eig_of_dpi(dpi: u16) -> Option<u32> {
    let dpi = if dpi == 23 { 22 } else { dpi as u32 };
    let mut eig = 0;
    while eig <= MOST_READ_EIG {
        if sprite_dpi_of_eig(eig) == dpi {
            return Some(eig as u32);
        }
        eig += 1;
    }
    None
}

/// The eigen factors sprite mode word `word`'s pixels are drawn at: a
/// numbered mode RISC OS 5 knows its own, an eigen-factor word's as stated,
/// and for each of a resolution word's resolutions the finest factor not
/// finer than it, rounding down. A mode variable read of such a word answers
/// only resolutions a factor names exactly. A numbered mode a module
/// describes is drawn at that mode's, as [`Pixels::shown`] holds them.
pub(crate) fn drawn_eig(word: u32) -> (u8, u8) {
    match reading(word) {
        SpriteModeWord::Number(_) => SpriteMode::from_value(word).map_or((1, 1), |mode| mode.eig()),
        SpriteModeWord::Dpi { dpi, .. } => (
            sprite_eig_of_dpi(u32::from(dpi.0)),
            sprite_eig_of_dpi(u32::from(dpi.1)),
        ),
        SpriteModeWord::Eig { eig, .. } => eig,
        SpriteModeWord::Selector => (1, 1),
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
            let format = numbered_format(os, number)?.map_err(|error| os.error(error))?;
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
            Ok((Pixels::typed(r6, sprite_type, wide_mask), r6))
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
            Ok((Pixels::typed(r6, sprite_type, wide_mask), r6))
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
/// describes for a number it does not know; else why no sprite is in it.
fn numbered_format(os: &mut Os<'_>, number: u8) -> Result<Result<Format, Error>, Fail> {
    if let Some(mode) = tairix_vdu::mode::numbered(u32::from(number & 0x7F)) {
        return Ok(if mode.graphic() && number < 0x80 {
            Ok(Format::numbered(mode))
        } else {
            Err(BAD_SPRITE_MODE)
        });
    }
    if number >= 0x80 {
        return Ok(Err(UNKNOWN_MODE));
    }
    let monitor = crate::state::with(os, |vdu, _| Ok(vdu.monitor))?;
    Ok(
        match select::resolve(os, &Spec::Number(u32::from(number)), monitor)? {
            Outcome::Valid(resolved) if resolved.format.graphic() => Ok(resolved.format),
            _ => Err(UNKNOWN_MODE),
        },
    )
}

/// The pixels a stored sprite's word gives, as [`Pixels::stored`] takes
/// them, a numbered mode RISC OS 5 does not know read from the module that
/// describes it, as RISC OS 5 reads one.
///
/// # Errors
///
/// As [`Pixels::stored`], and "Bad mode for sprite" for a number nothing
/// describes.
pub(crate) fn stored_pixels(os: &mut Os<'_>, word: u32) -> Result<Pixels, Fail> {
    match reading(word) {
        SpriteModeWord::Number(number) => match numbered_format(os, number)? {
            Ok(format) => Ok(Pixels::numbered(&format)),
            Err(_) => Err(os.error(BAD_SPRITE_MODE)),
        },
        _ => Pixels::stored(word).map_err(|error| os.error(error)),
    }
}

/// As [`Pixels::verified`] takes a word, a numbered mode RISC OS 5 does not
/// know read from the module that describes it.
pub(crate) fn verified_pixels(os: &mut Os<'_>, word: u32) -> Result<Pixels, Fail> {
    if let SpriteModeWord::Number(number) = reading(word) {
        if let Ok(format) = numbered_format(os, number)? {
            return Ok(Pixels::numbered(&format));
        }
    }
    Ok(Pixels::verified(word))
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
    let eig_fits = x_eig <= crate::mode::MOST_EIG && y_eig <= crate::mode::MOST_EIG;
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
