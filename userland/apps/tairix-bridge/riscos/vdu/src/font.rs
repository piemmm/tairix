//! The soft character set: a glyph for every character from 32 to 255,
//! each redefinable by `VDU 23`, reset by `OS_Byte` 20 and 25, and read by
//! `OS_Word` 10. They start as `lib/vdu`'s first-party font.

use tairix_vdu::font::{Glyph, FIRST, RISC_OS};

/// The characters the set holds.
const COUNT: usize = 256 - FIRST as usize;

/// The soft character set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Font {
    glyphs: [Glyph; COUNT],
}

impl Font {
    /// The system font.
    pub(crate) const fn new() -> Self {
        Self { glyphs: RISC_OS }
    }

    /// Character `code`'s glyph: nothing for a control code.
    pub(crate) fn glyph(&self, code: u8) -> Option<&Glyph> {
        self.glyphs.get(usize::from(code.checked_sub(FIRST)?))
    }

    /// Defines character `code` as `glyph`; a control code defines nothing.
    pub(crate) fn define(&mut self, code: u8, glyph: Glyph) {
        if let Some(slot) = code
            .checked_sub(FIRST)
            .and_then(|i| self.glyphs.get_mut(usize::from(i)))
        {
            *slot = glyph;
        }
    }

    /// Characters `first` to `last` as the system font draws them.
    pub(crate) fn reset(&mut self, first: u8, last: u8) {
        for code in first.max(FIRST)..=last {
            let i = usize::from(code - FIRST);
            self.glyphs[i] = RISC_OS[i];
        }
    }

    /// The character whose glyph is `rows`, if any is, the lowest first.
    pub(crate) fn find(&self, rows: Glyph) -> Option<u8> {
        (FIRST..=u8::MAX).find(|&code| self.glyph(code) == Some(&rows))
    }
}

/// The groups `OS_Byte` 25 resets: 0 every character from 32, else the 32
/// from 32 times the group.
pub(crate) const fn group(r1: u32) -> Option<(u8, u8)> {
    match r1 {
        0 => Some((32, 255)),
        1..=7 => {
            let first = 32 * r1.to_le_bytes()[0];
            Some((first, first + 31))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_definition_holds_until_its_group_is_reset() {
        let mut font = Font::new();
        let mine = [1, 2, 3, 4, 5, 6, 7, 8];
        font.define(b'A', mine);
        font.define(200, mine);
        font.define(10, mine);
        assert_eq!(font.glyph(b'A'), Some(&mine));
        assert_eq!(font.find(mine), Some(b'A'), "the lowest that matches");
        assert_eq!(font.glyph(10), None);
        let (first, last) = group(2).expect("a group");
        font.reset(first, last);
        assert_eq!(font.glyph(b'A'), Some(&RISC_OS[usize::from(b'A' - 32)]));
        assert_eq!(font.glyph(200), Some(&mine));
        assert_eq!(group(0), Some((32, 255)));
        assert_eq!(group(7), Some((224, 255)));
        assert_eq!(group(8), None);
    }
}
