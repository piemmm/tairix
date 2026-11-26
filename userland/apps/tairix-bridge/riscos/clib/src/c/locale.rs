//! `locale.h`'s `setlocale` and `localeconv`, and the collation `LC_COLLATE`
//! chooses: each category held as a territory's number, nought for the C
//! locale, with the names and composites RISC OS 5.30 accepts.

use tairix_riscos_arm::psr;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::{territory as territory_swi, X};
use tairix_riscos_kernel::Decimal;

use super::ctype::{self, bit};
use super::statics::{at, CATEGORY_COUNT, LCONV_BYTES, LOCALE_NAME_BYTES};
use super::stdio::stream;
use super::string;
use crate::kernel::context;
use crate::kernel::statics::Statics;

/// The categories in the order a composite names them: `LC_COLLATE`,
/// `LC_CTYPE`, `LC_MONETARY`, `LC_NUMERIC`, `LC_TIME`; a category's bit is
/// one shifted by its place.
const COLLATE: u32 = 0;
const CTYPE: u32 = 1;
const MONETARY: u32 = 2;
const NUMERIC: u32 = 3;
const TIME: u32 = 4;
const ALL: u32 = (1 << CATEGORY_COUNT) - 1;
/// The C locale's number, and the UK's, which `"ISO8859-1"` names.
const C: u32 = 0;
const UK: u32 = 1;
/// The territory number asking the Territory Manager for the current one.
const CURRENT: u32 = u32::MAX;
/// The longest name `setlocale` looks a territory up by.
const NAME_MOST: usize = 64;
/// The composite's mark.
const COMPOSITE: u8 = b'=';
/// What a `char` of `struct lconv` holds for "not available": `CHAR_MAX`.
const CHAR_MAX: u8 = 0xFF;
/// `struct lconv`'s strings, and `Territory_ReadSymbols`' symbol for each
/// field in its order: `LC_NUMERIC`'s three strings, `LC_MONETARY`'s seven
/// and its eight numbers after them.
const STRINGS: u32 = 10;
const NUMERIC_STRINGS: core::ops::Range<u32> = 0..3;
const MONETARY_STRINGS: core::ops::Range<u32> = 3..10;
const MONETARY_NUMBERS: core::ops::Range<u32> = 10..18;
const _: () = assert!(LCONV_BYTES == 4 * STRINGS + MONETARY_NUMBERS.end - MONETARY_NUMBERS.start);
/// The properties `Territory_CharacterPropertyTable` gives, and the bits
/// each sets; hexadecimal is taken only where not a digit, and a space not a
/// control character is blank.
const PROPERTIES: [(u32, u8); 7] = [
    (0, bit::CONTROL),
    (1, bit::UPPER),
    (2, bit::LOWER),
    (4, bit::PUNCT),
    (5, bit::SPACE),
    (6, bit::DIGIT),
    (7, bit::HEX),
];

/// How a `setlocale` request named its locale.
enum Named {
    /// One territory for every category asked, answered with the caller's
    /// own string.
    Territory(u32),
    /// The native territory, answered with its name in the library's buffer.
    Native(u32),
    /// A composite: a territory for each category by place.
    Composite([u32; CATEGORY_COUNT as usize]),
}

/// `setlocale(category, locale)`.
pub(super) fn setlocale(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let (category, locale) = (regs.r[0], regs.r[1]);
    if category == 0 || category & !ALL != 0 {
        regs.r[0] = 0;
        return Ok(());
    }
    if locale == 0 {
        regs.r[0] = write_composite(os, statics)?;
        return Ok(());
    }
    let Some(named) = named(os, locale)? else {
        regs.r[0] = 0;
        return Ok(());
    };
    let numbers = match named {
        Named::Territory(n) | Named::Native(n) => [n; CATEGORY_COUNT as usize],
        Named::Composite(numbers) => numbers,
    };
    for (place, &number) in (0..).zip(&numbers) {
        if category & (1 << place) != 0 {
            adopt(os, statics, place, number)?;
        }
    }
    regs.r[0] = match named {
        Named::Native(n) => write_name(os, statics, n)?,
        Named::Territory(_) | Named::Composite(_) => locale,
    };
    Ok(())
}

/// The locale `locale` names; none where it names none there is.
fn named(os: &mut Os<'_>, locale: u32) -> Result<Option<Named>, Fail> {
    let (name, ended) = os.as_caller().read_until(locale, NAME_MOST, |b| b == 0)?;
    if !ended {
        return Ok(None);
    }
    Ok(match name.as_slice() {
        b"" => {
            let mut regs = Regs::default();
            os.perform(X | territory_swi::NUMBER, &mut regs)?;
            Some(Named::Native(regs.r[0]))
        }
        b"C" => Some(Named::Territory(C)),
        b"ISO8859-1" => Some(Named::Native(UK)),
        [COMPOSITE, rest @ ..] => Some(Named::Composite(composite(rest))),
        _ => territory_named(os, locale)?.map(Named::Territory),
    })
}

/// A composite's numbers by place, as many as it gives before anything but
/// digits and commas; the rest nought.
fn composite(text: &[u8]) -> [u32; CATEGORY_COUNT as usize] {
    let mut numbers = [0; CATEGORY_COUNT as usize];
    let mut rest = text;
    for number in &mut numbers {
        let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        *number = rest[..digits].iter().fold(0u32, |n, &d| {
            n.wrapping_mul(10).wrapping_add(u32::from(d - b'0'))
        });
        match rest[digits..].split_first() {
            Some((b',', after)) => rest = after,
            _ => break,
        }
    }
    numbers
}

/// The number of the territory loaded by the name at `locale`; none for a
/// name no loaded territory has.
fn territory_named(os: &mut Os<'_>, locale: u32) -> Result<Option<u32>, Fail> {
    let mut regs = stream::registers(&[CURRENT, locale]);
    if !os.perform_quietly(X | territory_swi::NAME_TO_NUMBER, &mut regs)? || regs.r[0] == 0 {
        return Ok(None);
    }
    let number = regs.r[0];
    let mut exists = Regs::default();
    exists.r[0] = number;
    os.perform(X | territory_swi::EXISTS, &mut exists)?;
    Ok((exists.flags & psr::Z != 0).then_some(number))
}

/// Category `place` set to territory `number`, and what it governs made to
/// follow.
fn adopt(os: &mut Os<'_>, statics: Statics, place: u32, number: u32) -> Result<(), Fail> {
    statics.write(os, at::CATEGORIES + 4 * place, number)?;
    match place {
        CTYPE => adopt_ctype(os, statics, number),
        NUMERIC => adopt_strings(os, statics, number, NUMERIC_STRINGS),
        MONETARY => {
            adopt_strings(os, statics, number, MONETARY_STRINGS)?;
            adopt_numbers(os, statics, number)
        }
        _ => Ok(()),
    }
}

/// The `ctype` table and the case tables for territory `number`. A table a
/// territory cannot give is left out, as RISC OS 5.30 leaves it.
fn adopt_ctype(os: &mut Os<'_>, statics: Statics, number: u32) -> Result<(), Fail> {
    let (table, lower, upper) = if number == C {
        (ctype::C_LOCALE, 0, 0)
    } else {
        let mut table = [0; 256];
        for (property, bits) in PROPERTIES {
            if let Some(map) = property_map(os, number, property)? {
                for (c, class) in table.iter_mut().enumerate() {
                    if map[c >> 3] & (1 << (c & 7)) != 0 {
                        *class |= bits;
                    }
                }
            }
        }
        for class in &mut table {
            if *class & bit::DIGIT != 0 {
                *class &= !bit::HEX;
            }
            if *class & (bit::SPACE | bit::CONTROL) == bit::SPACE {
                *class |= bit::BLANK;
            }
        }
        let lower = asked(os, territory_swi::LOWER_CASE_TABLE, &[number])?.unwrap_or(0);
        let upper = asked(os, territory_swi::UPPER_CASE_TABLE, &[number])?.unwrap_or(0);
        (table, lower, upper)
    };
    os.as_caller()
        .write_bytes(statics.field(at::CTYPE), &table)?;
    statics.write(os, at::LOWER_TABLE, lower)?;
    statics.write(os, at::UPPER_TABLE, upper)
}

/// Territory `number`'s map of `property`: a bit for each character.
fn property_map(os: &mut Os<'_>, number: u32, property: u32) -> Result<Option<[u8; 32]>, Fail> {
    let Some(at) = asked(
        os,
        territory_swi::CHARACTER_PROPERTY_TABLE,
        &[number, property],
    )?
    else {
        return Ok(None);
    };
    let mut map = [0; 32];
    os.as_caller().read_bytes(at, &mut map)?;
    Ok(Some(map))
}

/// R0 of `swi` performed with `low` from R0 on; none where it fails.
fn asked(os: &mut Os<'_>, swi: u32, low: &[u32]) -> Result<Option<u32>, Fail> {
    let mut regs = Regs::default();
    regs.r[..low.len()].copy_from_slice(low);
    Ok(os.perform_quietly(X | swi, &mut regs)?.then_some(regs.r[0]))
}

/// `struct lconv`'s strings in `fields`: territory `number`'s symbols, or
/// the C locale's.
fn adopt_strings(
    os: &mut Os<'_>,
    statics: Statics,
    number: u32,
    fields: core::ops::Range<u32>,
) -> Result<(), Fail> {
    let empty = statics.field(at::C_STRINGS + 1);
    for field in fields {
        let c_string = if field == 0 {
            statics.field(at::C_STRINGS)
        } else {
            empty
        };
        let string = match number {
            C => c_string,
            _ => asked(os, territory_swi::READ_SYMBOLS, &[number, field])?.unwrap_or(c_string),
        };
        statics.write(os, at::LCONV + 4 * field, string)?;
    }
    Ok(())
}

/// `struct lconv`'s monetary numbers: territory `number`'s symbols, or
/// `CHAR_MAX` for the C locale's.
fn adopt_numbers(os: &mut Os<'_>, statics: Statics, number: u32) -> Result<(), Fail> {
    for (field, symbol) in (0..).zip(MONETARY_NUMBERS) {
        let value = match number {
            C => None,
            _ => asked(os, territory_swi::READ_SYMBOLS, &[number, symbol])?,
        };
        let byte = value.map_or(CHAR_MAX, |v| v.to_le_bytes()[0]);
        os.as_caller()
            .write_byte(statics.field(at::LCONV + 4 * STRINGS + field), byte)?;
    }
    Ok(())
}

/// The C locale in every category, as the library starts.
pub(crate) fn start_in_c(os: &mut Os<'_>, statics: Statics) -> Result<(), Fail> {
    os.as_caller()
        .write_bytes(statics.field(at::C_STRINGS), b".\0\0\0")?;
    for place in 0..CATEGORY_COUNT {
        adopt(os, statics, place, C)?;
    }
    Ok(())
}

/// The composite naming every category's territory, in the library's
/// buffer: its address.
fn write_composite(os: &mut Os<'_>, statics: Statics) -> Result<u32, Fail> {
    let mut text = alloc::vec::Vec::new();
    text.try_reserve(LOCALE_NAME_BYTES as usize)
        .map_err(|_| os.no_memory())?;
    text.push(COMPOSITE);
    for place in 0..CATEGORY_COUNT {
        if place > 0 {
            text.push(b',');
        }
        let number = statics.read(os, at::CATEGORIES + 4 * place)?;
        text.extend_from_slice(Decimal::of(number.cast_signed()).as_bytes());
    }
    let buffer = statics.field(at::LOCALE_NAME);
    os.as_caller()
        .write_terminated(buffer, &text, 0, LOCALE_NAME_BYTES as usize)?;
    Ok(buffer)
}

/// Territory `number`'s name in the library's buffer: its address, or
/// nought where the territory has no name.
fn write_name(os: &mut Os<'_>, statics: Statics, number: u32) -> Result<u32, Fail> {
    let buffer = statics.field(at::LOCALE_NAME);
    let named = asked(
        os,
        territory_swi::NUMBER_TO_NAME,
        &[number, buffer, LOCALE_NAME_BYTES],
    )?;
    Ok(named.map_or(0, |_| buffer))
}

/// `localeconv()`.
pub(super) fn localeconv(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    regs.r[0] = statics.field(at::LCONV);
    Ok(())
}

/// The territory the category at `place` is in, nought for the C locale.
fn territory_of(os: &mut Os<'_>, statics: Statics, place: u32) -> Result<u32, Fail> {
    statics.read(os, at::CATEGORIES + 4 * place)
}

fn collate_territory(os: &mut Os<'_>, regs: &Regs) -> Result<u32, Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    territory_of(os, statics, COLLATE)
}

/// `LC_TIME`'s territory; none in the C locale.
pub(super) fn time_territory(os: &mut Os<'_>, statics: Statics) -> Result<Option<u32>, Fail> {
    Ok(Some(territory_of(os, statics, TIME)?).filter(|&number| number != C))
}

/// `strcoll(s1, s2)`: `strcmp`'s answer in the C locale, the territory's
/// collation otherwise.
pub(super) fn strcoll(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match collate_territory(os, regs)? {
        C => string::strcmp(os, regs),
        number => {
            let mut collate = stream::registers(&[number, regs.r[0], regs.r[1], 0]);
            os.perform(X | territory_swi::COLLATE, &mut collate)?;
            regs.r[0] = collate.r[0];
            Ok(())
        }
    }
}

/// `strxfrm(s1, s2, n)`: `s2` itself in the C locale, its territory form
/// otherwise; written only where it fits, its length the answer either way.
pub(super) fn strxfrm(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (to, from, room) = (regs.r[0], regs.r[1], regs.r[2]);
    regs.r[0] = match collate_territory(os, regs)? {
        C => {
            let length = string::length(os, from)?;
            if length < room {
                os.as_caller().copy_bytes(from, to, length + 1)?;
            }
            length
        }
        number => {
            let mut transform = stream::registers(&[number, to, from, room]);
            os.perform(X | territory_swi::TRANSFORM_STRING, &mut transform)?;
            transform.r[0]
        }
    };
    Ok(())
}

#[cfg(test)]
mod tests;
