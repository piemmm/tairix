//! PostScript glyph names and the characters they stand for, as RISC OS's
//! UTF8 encoding has them: the names of the Adobe Glyph List and the ITC Zapf
//! Dingbats Glyph List, a `uniXXXX` name's character and a `uniXXXXYYYY`
//! name's surrogate pair. A name is matched exactly. A character is named as
//! the Adobe Glyph List For New Fonts recommends, else as the Glyph List
//! first names it. The three lists in `assets/` are Adobe's byte for byte.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const LIST: &str = include_str!("../assets/glyphlist.txt");
const DINGBATS: &str = include_str!("../assets/zapfdingbats.txt");
const NEW_FONTS: &str = include_str!("../assets/aglfn.txt");

/// UTF-16's surrogates, high then low, which a `uniXXXXYYYY` name and a
/// string of halfwords pair.
pub(crate) const HIGH_SURROGATES: core::ops::RangeInclusive<u32> = 0xD800..=0xDBFF;
pub(crate) const LOW_SURROGATES: core::ops::RangeInclusive<u32> = 0xDC00..=0xDFFF;

/// The character the surrogates `high` and `low` pair.
pub(crate) const fn paired(high: u32, low: u32) -> u32 {
    0x1_0000 + ((high - *HIGH_SURROGATES.start()) << 10) + (low - *LOW_SURROGATES.start())
}

/// The glyph lists, read.
pub(crate) struct GlyphNames {
    /// Each name standing for one character, by name, the lists' order kept
    /// among a name's characters.
    by_name: Vec<(&'static str, u32)>,
    /// The name each character goes by, by character.
    by_character: Vec<(u32, &'static str)>,
}

/// A list's `name;XXXX` lines; a line naming a sequence of characters names
/// no one character and is passed over.
fn entries(list: &'static str) -> impl Iterator<Item = (&'static str, u32)> {
    list.lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let (name, codes) = line.split_once(';')?;
            if codes.contains(' ') {
                return None;
            }
            Some((name, u32::from_str_radix(codes, 16).ok()?))
        })
}

impl GlyphNames {
    pub(crate) fn read() -> Self {
        let mut by_name: Vec<(&str, u32)> = entries(LIST).chain(entries(DINGBATS)).collect();
        let recommended = NEW_FONTS
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| {
                let mut fields = line.split(';');
                let character = u32::from_str_radix(fields.next()?, 16).ok()?;
                Some((character, fields.next()?))
            });
        let mut by_character: Vec<(u32, &str)> = recommended
            .chain(entries(LIST).map(|(name, character)| (character, name)))
            .collect();
        by_character.sort_by_key(|&(character, _)| character);
        by_character.dedup_by_key(|&mut (character, _)| character);
        by_name.sort_by(|a, b| a.0.cmp(b.0));
        Self {
            by_name,
            by_character,
        }
    }

    /// The characters `name` stands for: the lists', else its `uni`
    /// spelling's.
    pub(crate) fn characters(&self, name: &[u8]) -> Vec<u32> {
        let Ok(name) = core::str::from_utf8(name) else {
            return Vec::new();
        };
        let start = self.by_name.partition_point(|&(n, _)| n < name);
        let listed: Vec<u32> = self.by_name[start..]
            .iter()
            .take_while(|&&(n, _)| n == name)
            .map(|&(_, character)| character)
            .collect();
        if !listed.is_empty() {
            return listed;
        }
        spelled(name).into_iter().collect()
    }

    /// The name the lists give `character`, if they name it.
    pub(crate) fn listed(&self, character: u32) -> Option<&'static str> {
        self.by_character
            .binary_search_by_key(&character, |&(c, _)| c)
            .ok()
            .map(|at| self.by_character[at].1)
    }

    /// The name `character` goes by: the list's for it, else its `uni`
    /// spelling, a surrogate pair past the Basic Multilingual Plane.
    pub(crate) fn name(&self, character: u32) -> String {
        if let Some(listed) = self.listed(character) {
            return String::from(listed);
        }
        match character.checked_sub(0x1_0000) {
            Some(above) => format!(
                "uni{:04X}{:04X}",
                *HIGH_SURROGATES.start() + (above >> 10),
                *LOW_SURROGATES.start() + (above & 0x3FF)
            ),
            None => format!("uni{character:04X}"),
        }
    }
}

/// The character a `uniXXXX` name spells, or the one a `uniXXXXYYYY` name's
/// surrogate pair does: upper-case hexadecimal, a lone surrogate none.
fn spelled(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("uni")?;
    if !digits
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
    {
        return None;
    }
    let unit = |hex: &str| u32::from_str_radix(hex, 16).ok();
    let character = match digits.len() {
        4 => unit(digits)?,
        8 => {
            let (high, low) = (unit(&digits[..4])?, unit(&digits[4..])?);
            if !HIGH_SURROGATES.contains(&high) || !LOW_SURROGATES.contains(&low) {
                return None;
            }
            paired(high, low)
        }
        _ => return None,
    };
    char::from_u32(character).map(u32::from)
}

#[cfg(test)]
mod tests;
