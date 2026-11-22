//! The palette (`plans/RISCOS-EMULATOR.md` §8): each logical colour's two
//! flash states, the border and the pointer's three colours, as `&BBGGRRxx`
//! words whose low byte says how each was set; the 256-colour modes' VIDC1
//! groups of sixteen; and the direct-colour modes' gamma ramp. `VDU 19`,
//! `OS_Word` 11 and 12 and `OS_ReadPalette` go through `PaletteV`, whose
//! default is here, so a claimant may stand in front of it.

use tairix_colour::Rgb;
use tairix_riscos_kernel::os::{Entry, Fail, Os, Regs};
use tairix_riscos_kernel::vectors::Vector;
use tairix_vdu::mode::Flags;
use tairix_vdu::palette::{default_256, DEFAULT_2, DEFAULT_4};

use crate::mode::Format;
use crate::state::with;

/// `PaletteV`.
pub(crate) const PALETTE_V: Vector = Vector(0x23);

/// How an entry was set, its word's low byte.
pub(crate) mod kind {
    /// Both flash states alike.
    pub(crate) const NORMAL: u32 = 16;
    /// The first flash state alone.
    pub(super) const FIRST: u32 = 17;
    /// The second alone.
    pub(super) const SECOND: u32 = 18;
    /// The border.
    pub(super) const BORDER: u32 = 24;
    /// One of the pointer's colours.
    pub(crate) const POINTER: u32 = 25;
    /// The supremacy bit.
    pub(super) const SUPREMACY: u32 = 0x80;
}

/// `PaletteV`'s reasons.
pub(crate) mod reason {
    pub(crate) const READ: u32 = 1;
    pub(crate) const SET: u32 = 2;
    pub(super) const FIRST_FLASH: u32 = 3;
    pub(super) const SECOND_FLASH: u32 = 4;
    pub(super) const DEFAULT: u32 = 5;
    pub(super) const BLANK: u32 = 6;
    pub(super) const READ_BLOCK: u32 = 7;
    pub(super) const WRITE_BLOCK: u32 = 8;
    pub(super) const GAMMA: u32 = 9;
}

/// The BBC Micro's sixteen physical colours: eight solid, then eight
/// flashing pairs.
const PHYSICAL: [[Rgb; 2]; 16] = {
    const SOLID: [Rgb; 8] = [
        Rgb::new(0, 0, 0),
        Rgb::new(0xFF, 0, 0),
        Rgb::new(0, 0xFF, 0),
        Rgb::new(0xFF, 0xFF, 0),
        Rgb::new(0, 0, 0xFF),
        Rgb::new(0xFF, 0, 0xFF),
        Rgb::new(0, 0xFF, 0xFF),
        Rgb::new(0xFF, 0xFF, 0xFF),
    ];
    let mut table = [[SOLID[0]; 2]; 16];
    let mut i = 0;
    while i < 8 {
        table[i] = [SOLID[i], SOLID[i]];
        table[i + 8] = [SOLID[i], SOLID[7 - i]];
        i += 1;
    }
    table
};

/// A colour word: `rgb` with `how` in the low byte.
const fn word(rgb: Rgb, how: u32) -> u32 {
    rgb.riscos_word() | how
}

/// Physical colour `physical`'s solid state.
const fn physical(physical: u8) -> Rgb {
    PHYSICAL[physical as usize][0]
}

/// The palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Palette {
    /// Each logical colour's first and second flash states; the direct
    /// modes' gamma ramp.
    pub(crate) entries: [[u32; 2]; 256],
    /// The VIDC1 groups' sixteen base colours.
    base: [[u32; 2]; 16],
    pub(crate) border: [u32; 2],
    pub(crate) pointer: [[u32; 2]; 3],
    /// The second flash state is showing.
    pub(crate) second: bool,
    /// The screen is blanked.
    pub(crate) blanked: bool,
}

impl Palette {
    /// The palette a mode starts with.
    pub(crate) fn for_mode(format: &Format) -> Self {
        let mut palette = Self {
            entries: [[0; 2]; 256],
            base: [[0; 2]; 16],
            border: [word(physical(0), kind::BORDER); 2],
            pointer: [[word(physical(0), kind::POINTER); 2]; 3],
            second: false,
            blanked: false,
        };
        palette.set_default(format);
        palette
    }

    /// The mode's default entries; the border and the pointer's colours as
    /// they were.
    pub(crate) fn set_default(&mut self, format: &Format) {
        let solid = |rgb: Rgb| [word(rgb, kind::NORMAL); 2];
        self.entries = [[0; 2]; 256];
        match format.log2_bpp() {
            0 => {
                for (entry, &p) in self.entries.iter_mut().zip(&DEFAULT_2) {
                    *entry = solid(physical(p));
                }
            }
            1 => {
                for (entry, &p) in self.entries.iter_mut().zip(&DEFAULT_4) {
                    *entry = solid(physical(p));
                }
            }
            2 => {
                for (i, entry) in self.entries.iter_mut().take(16).enumerate() {
                    let [first, second] = PHYSICAL[i];
                    *entry = if i < 8 {
                        solid(first)
                    } else if format.has(Flags::TELETEXT) {
                        [word(first, kind::NORMAL | kind::SUPREMACY); 2]
                    } else {
                        [word(first, kind::FIRST), word(second, kind::SECOND)]
                    };
                }
            }
            3 => {
                for (i, entry) in self.entries.iter_mut().enumerate() {
                    let [r, g, b] = default_256(i.to_le_bytes()[0]);
                    *entry = solid(Rgb::new(r, g, b));
                }
                for (i, base) in self.base.iter_mut().enumerate() {
                    *base = self.entries[i];
                }
            }
            _ => {
                for (i, entry) in (0..=u8::MAX).zip(self.entries.iter_mut()) {
                    *entry = solid(Rgb::new(i, i, i));
                }
            }
        }
        self.second = false;
    }

    /// Entry `logical`'s words of `kind`: a logical colour's, the border's or
    /// one of the pointer's.
    fn read(&self, format: &Format, logical: u32, how: u32) -> Option<[u32; 2]> {
        match how {
            kind::BORDER => Some(self.border),
            kind::POINTER => self.pointer.get(logical.checked_sub(1)? as usize).copied(),
            _ => Some(self.entries[(logical & colours(format)) as usize]),
        }
    }

    /// Sets entry `logical`'s words of `kind`, a 256-colour mode's group of
    /// sixteen together.
    fn write(&mut self, format: &Format, logical: u32, how: u32, words: [u32; 2]) {
        match how {
            kind::BORDER => self.border = words,
            kind::POINTER => {
                if let Some(entry) = logical
                    .checked_sub(1)
                    .and_then(|n| self.pointer.get_mut(n as usize))
                {
                    *entry = words;
                }
            }
            _ if format.grouped() => {
                let base = logical & 15;
                self.base[base as usize] = words;
                for (entry, i) in self
                    .entries
                    .iter_mut()
                    .zip(0u32..)
                    .skip(base as usize)
                    .step_by(16)
                {
                    *entry = words.map(|w| grouped(w, i));
                }
            }
            _ => self.entries[(logical & colours(format)) as usize] = words,
        }
    }

    /// The colour of pixel value `pixel` as the display shows it now.
    pub(crate) fn shown(&self, pixel: u32) -> Rgb {
        Rgb::from_riscos_word(self.entries[(pixel & 0xFF) as usize][usize::from(self.second)])
    }

    /// Whether any entry flashes.
    pub(crate) fn flashing(&self, format: &Format) -> bool {
        let count = (colours(format) + 1) as usize;
        self.entries[..count]
            .iter()
            .any(|&[first, second]| Rgb::from_riscos_word(first) != Rgb::from_riscos_word(second))
    }
}

/// The highest entry a mode's logical colours reach.
const fn colours(format: &Format) -> u32 {
    if format.log2_bpp() >= 3 {
        255
    } else {
        format.ncolour()
    }
}

/// A base colour word as entry `i` of its group shows it, its low byte kept.
const fn grouped(base: u32, i: u32) -> u32 {
    let [r, g, b] =
        tairix_vdu::palette::grouped(Rgb::from_riscos_word(base).to_array(), i.to_le_bytes()[0]);
    word(Rgb::new(r, g, b), base & 0xFF)
}

/// What `VDU 19,logical,physical,red,green,blue` sets: the kind of entry and
/// its two flash states, the state not set kept from `current`. `None` for a
/// physical colour no form names.
pub(crate) fn vdu_19(physical: u8, rgb: [u8; 3], current: [u32; 2]) -> Option<(u32, [u32; 2])> {
    let supremacy = u32::from(physical & 0x80);
    let given = Rgb::new(rgb[0], rgb[1], rgb[2]);
    let mark = |how: u32| how | supremacy;
    match u32::from(physical & 0x7F) {
        p @ 0..=15 => {
            let [first, second] = PHYSICAL[p as usize];
            Some(if p < 8 {
                (kind::NORMAL, [word(first, mark(kind::NORMAL)); 2])
            } else {
                (
                    kind::NORMAL,
                    [
                        word(first, mark(kind::FIRST)),
                        word(second, mark(kind::SECOND)),
                    ],
                )
            })
        }
        kind::NORMAL => Some((kind::NORMAL, [word(given, mark(kind::NORMAL)); 2])),
        kind::FIRST => Some((kind::NORMAL, [word(given, mark(kind::FIRST)), current[1]])),
        kind::SECOND => Some((kind::NORMAL, [current[0], word(given, mark(kind::SECOND))])),
        kind::BORDER => Some((kind::BORDER, [word(given, mark(kind::BORDER)); 2])),
        kind::POINTER => Some((kind::POINTER, [word(given, mark(kind::POINTER)); 2])),
        _ => None,
    }
}

/// An entry's words as they are now, for a `VDU 19` that keeps one state.
pub(crate) fn current(palette: &Palette, format: &Format, logical: u32, how: u32) -> [u32; 2] {
    palette.read(format, logical, how).unwrap_or([0; 2])
}

/// `VDU 19` through `PaletteV`: what it sets worked out, then set by
/// whatever stands on the vector.
pub(crate) fn write_through(
    os: &mut Os<'_>,
    logical: u8,
    physical: u8,
    rgb: [u8; 3],
) -> Result<(), Fail> {
    let how = match physical & 0x7F {
        24 => kind::BORDER,
        25 => kind::POINTER,
        _ => kind::NORMAL,
    };
    let set = with(os, |vdu, _| {
        let format = vdu.screen.format;
        Ok(vdu_19(
            physical,
            rgb,
            current(&vdu.palette, &format, u32::from(logical), how),
        ))
    })?;
    let Some((how, [first, second])) = set else {
        return Ok(());
    };
    let mut regs = Regs::new([
        u32::from(logical),
        how,
        first,
        second,
        reason::SET,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]);
    os.call_vector(PALETTE_V, &mut regs, Entry::Supervisor)?;
    Ok(())
}

/// `VDU 20`'s palette through `PaletteV`.
pub(crate) fn default_through(os: &mut Os<'_>) -> Result<(), Fail> {
    let mut regs = Regs::new([0, 0, 0, 0, reason::DEFAULT, 0, 0, 0, 0, 0, 0, 0, 0]);
    os.call_vector(PALETTE_V, &mut regs, Entry::Supervisor)?;
    Ok(())
}

/// `PaletteV`'s default: the drivers' own palette. A reason it knows is
/// claimed by R4 nought; any other passes on untouched.
pub(crate) fn vector(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let handled = match regs.r[4] {
        reason::READ => with(os, |vdu, _| {
            let format = vdu.screen.format;
            let [first, second] = vdu
                .palette
                .read(&format, regs.r[0], regs.r[1])
                .unwrap_or([0, 0]);
            regs.r[2] = first;
            regs.r[3] = second;
            Ok(true)
        })?,
        reason::SET => with(os, |vdu, _| {
            let format = vdu.screen.format;
            vdu.palette
                .write(&format, regs.r[0], regs.r[1], [regs.r[2], regs.r[3]]);
            Ok(true)
        })?,
        reason::FIRST_FLASH | reason::SECOND_FLASH => with(os, |vdu, _| {
            vdu.palette.second = regs.r[4] == reason::SECOND_FLASH;
            Ok(true)
        })?,
        reason::DEFAULT => with(os, |vdu, _| {
            let format = vdu.screen.format;
            vdu.palette.set_default(&format);
            Ok(true)
        })?,
        reason::BLANK => with(os, |vdu, _| {
            let was = u32::from(vdu.palette.blanked);
            match regs.r[0] {
                0 => vdu.palette.blanked = false,
                1 => vdu.palette.blanked = true,
                _ => {}
            }
            regs.r[0] = was;
            Ok(true)
        })?,
        reason::READ_BLOCK | reason::WRITE_BLOCK => block(os, regs)?,
        reason::GAMMA => gamma(os, regs)?,
        _ => false,
    };
    if handled {
        regs.r[4] = 0;
    }
    Ok(())
}

/// `PaletteV` 7 and 8: many entries read or written at once. R0 the list of
/// logical colours, or nought for those from R1's own count up; R1 the count
/// in its low 24 bits and the kind in its top eight; R2 and R3 the first and
/// second states' words, either nought where not wanted.
fn block(os: &mut Os<'_>, regs: &mut Regs) -> Result<bool, Fail> {
    let (list, count, how) = (regs.r[0], regs.r[1] & 0xFF_FFFF, regs.r[1] >> 24);
    let (firsts, seconds) = (regs.r[2], regs.r[3]);
    let write = regs.r[4] == reason::WRITE_BLOCK;
    for i in 0..count.min(256) {
        let logical = if list == 0 {
            i
        } else {
            os.read_word(list.wrapping_add(4 * i))?
        };
        let at = |base: u32| base.wrapping_add(4 * i);
        if write {
            let first = if firsts == 0 {
                None
            } else {
                Some(os.read_word(at(firsts))?)
            };
            let second = if seconds == 0 {
                None
            } else {
                Some(os.read_word(at(seconds))?)
            };
            with(os, |vdu, _| {
                let format = vdu.screen.format;
                let [old_first, old_second] = current(&vdu.palette, &format, logical, how);
                vdu.palette.write(
                    &format,
                    logical,
                    how,
                    [first.unwrap_or(old_first), second.unwrap_or(old_second)],
                );
                Ok(())
            })?;
        } else {
            let [first, second] = with(os, |vdu, _| {
                let format = vdu.screen.format;
                Ok(current(&vdu.palette, &format, logical, how))
            })?;
            if firsts != 0 {
                os.write_word(at(firsts), first)?;
            }
            if seconds != 0 {
                os.write_word(at(seconds), second)?;
            }
        }
    }
    Ok(true)
}

/// `PaletteV` 9: gamma tables for red, green and blue at R0, R1 and R2, 256
/// bytes each, which a direct-colour mode's ramp is.
fn gamma(os: &mut Os<'_>, regs: &mut Regs) -> Result<bool, Fail> {
    let mut tables = [[0u8; 256]; 3];
    for (table, &at) in tables.iter_mut().zip(&regs.r[..3]) {
        os.read_bytes(at, table)?;
    }
    with(os, |vdu, _| {
        if vdu.screen.format.direct() {
            for (i, entry) in vdu.palette.entries.iter_mut().enumerate() {
                let rgb = Rgb::new(tables[0][i], tables[1][i], tables[2][i]);
                *entry = [word(rgb, kind::NORMAL); 2];
            }
        }
        Ok(true)
    })
}

#[cfg(test)]
mod tests;
