//! The palette (`plans/RISCOS-EMULATOR.md` §8): each logical colour's two
//! flash states, the border and the pointer's three colours, as `&BBGGRRxx`
//! words held as they were set, a colour of supremacy's low byte all ones;
//! the 256-colour modes' VIDC1 groups of sixteen; and the direct-colour
//! modes' gamma ramp. Reading one entry reports in its words' low bytes how
//! it shows, as RISC OS 5's `OS_ReadPalette` does; reading a block gives the
//! words as held. `VDU 19`,
//! `OS_Word` 11 and 12 and `OS_ReadPalette` go through `PaletteV`, whose
//! default is here, so a claimant may stand in front of it.

use tairix_colour::Rgb;
use tairix_riscos_kernel::os::{len32, Entry, Fail, Os, Regs};
use tairix_vdu::mode::Flags;
use tairix_vdu::palette::{default_256, DEFAULT_2, DEFAULT_4};

use crate::mode::Format;
use crate::state::with;

pub(crate) use tairix_riscos_kernel::vectors::PALETTE as PALETTE_V;

/// How an entry shows, its reported word's low byte.
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

/// The low byte a colour of supremacy is held with.
const SUPREME: u32 = 0xFF;

/// `PaletteV`'s reasons.
pub(crate) mod reason {
    pub(crate) const READ: u32 = 1;
    pub(crate) const SET: u32 = 2;
    pub(super) const FIRST_FLASH: u32 = 3;
    pub(super) const SECOND_FLASH: u32 = 4;
    pub(super) const DEFAULT: u32 = 5;
    pub(super) const BLANK: u32 = 6;
    pub(crate) const READ_BLOCK: u32 = 7;
    pub(crate) const WRITE_BLOCK: u32 = 8;
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

/// A colour word as held: `rgb`, its low byte all ones where it has
/// supremacy.
const fn word(rgb: Rgb, supreme: bool) -> u32 {
    rgb.riscos_word() | if supreme { SUPREME } else { 0 }
}

/// What reading entry `how` reports of its held words: each word's low byte
/// its supremacy bit and how the entry shows, both states alike or which
/// state it is.
const fn reported(how: u32, [first, second]: [u32; 2]) -> [u32; 2] {
    match how {
        kind::BORDER | kind::POINTER => [marked(first, how), marked(second, how)],
        _ if first == second => [marked(first, kind::NORMAL), marked(second, kind::NORMAL)],
        _ => [marked(first, kind::FIRST), marked(second, kind::SECOND)],
    }
}

/// A held word reported as showing `how`, its supremacy bit kept.
const fn marked(word: u32, how: u32) -> u32 {
    (word & !0xFF) | (word & kind::SUPREMACY) | how
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
    pub(crate) entries: [[u32; 2]; EIGHT_BIT_COLOURS],
    /// The VIDC1 groups' base colours.
    base: [[u32; 2]; GROUP_BASES],
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
            entries: [[0; 2]; EIGHT_BIT_COLOURS],
            base: [[0; 2]; GROUP_BASES],
            border: [word(physical(0), false); 2],
            pointer: [[word(physical(0), false); 2]; 3],
            second: false,
            blanked: false,
        };
        palette.set_default(format);
        palette
    }

    /// The mode's default entries; the border and the pointer's colours as
    /// they were.
    pub(crate) fn set_default(&mut self, format: &Format) {
        self.entries = default_words(format.log2_bpp(), format.has(Flags::TELETEXT));
        if format.log2_bpp() == 3 {
            for (i, base) in self.base.iter_mut().enumerate() {
                *base = self.entries[i];
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
                let base = logical as usize % GROUP_BASES;
                self.base[base] = words;
                for (entry, i) in self
                    .entries
                    .iter_mut()
                    .zip(0u32..)
                    .skip(base)
                    .step_by(GROUP_BASES)
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

/// A mode of `log2_bpp`'s first `entries` default colours, each in its first
/// flash state.
#[cfg(test)]
pub(crate) fn default_colours(log2_bpp: u32, entries: usize) -> alloc::vec::Vec<Rgb> {
    default_words(log2_bpp, false)
        .iter()
        .take(entries)
        .map(|&[first, _]| Rgb::from_riscos_word(first))
        .collect()
}

/// The entries a mode of `log2_bpp` starts with, each logical colour's two
/// flash states; a Teletext mode's flashing colours steady, with the
/// supremacy bit; the direct-colour modes' gamma ramp.
pub(crate) fn default_words(log2_bpp: u32, teletext: bool) -> [[u32; 2]; EIGHT_BIT_COLOURS] {
    let solid = |rgb: Rgb| [word(rgb, false); 2];
    let mut entries = [[0; 2]; EIGHT_BIT_COLOURS];
    match log2_bpp {
        0 => {
            for (entry, &p) in entries.iter_mut().zip(&DEFAULT_2) {
                *entry = solid(physical(p));
            }
        }
        1 => {
            for (entry, &p) in entries.iter_mut().zip(&DEFAULT_4) {
                *entry = solid(physical(p));
            }
        }
        2 => {
            for (i, entry) in entries.iter_mut().take(16).enumerate() {
                let [first, second] = PHYSICAL[i];
                *entry = if i < 8 {
                    solid(first)
                } else if teletext {
                    [word(first, true); 2]
                } else {
                    [word(first, false), word(second, false)]
                };
            }
        }
        3 => {
            for (pixel, entry) in (0..=u8::MAX).zip(entries.iter_mut()) {
                let [r, g, b] = default_256(pixel);
                *entry = solid(Rgb::new(r, g, b));
            }
        }
        _ => {
            for (i, entry) in (0..=u8::MAX).zip(entries.iter_mut()) {
                *entry = solid(Rgb::new(i, i, i));
            }
        }
    }
    entries
}

/// The highest entry a mode's logical colours reach.
const fn colours(format: &Format) -> u32 {
    if format.log2_bpp() >= 3 {
        255
    } else {
        format.ncolour()
    }
}

/// The colours of a palette of eight bits a pixel.
pub(crate) const EIGHT_BIT_COLOURS: usize = 1 << 8;

/// Whether `colours` are the default 256-colour palette's, each channel
/// compared in the bits `kept` holds.
pub(crate) fn is_default_256(colours: &[Rgb], kept: u8) -> bool {
    colours.len() == EIGHT_BIT_COLOURS
        && colours.iter().zip(0..=u8::MAX).all(|(colour, pixel)| {
            colour.to_array().map(|channel| channel & kept)
                == default_256(pixel).map(|channel| channel & kept)
        })
}
/// The base colours a grouped 256-colour palette makes its colours from:
/// VIDC1's palette registers, a pixel's low four bits choosing one.
pub(crate) const GROUP_BASES: usize = 16;

pub(crate) use tairix_vdu::palette::VIDC1_CHANNEL;

/// The colour pixel `pixel` of a 256-colour mode whose palette is grouped
/// shows from base colour `base` at VIDC1's four bits a channel, as RISC OS
/// 5's ColourTrans and sprite palettes take it: each channel's
/// [`VIDC1_CHANNEL`] bits, repeated below.
const fn vidc1_shown(base: [u8; 3], pixel: u8) -> [u8; 3] {
    use tairix_vdu::palette::{grouped, vidc1_channel};
    let [red, green, blue] = base;
    grouped(
        [
            vidc1_channel(red),
            vidc1_channel(green),
            vidc1_channel(blue),
        ],
        pixel,
    )
}

/// The 256 colours a grouped palette of base words `bases` shows, as VIDC1
/// shows them, a base it lacks black.
pub(crate) fn grouped_colours(bases: &[u32]) -> impl Iterator<Item = Rgb> + '_ {
    (0..=u8::MAX).map(|pixel| {
        let base = bases
            .get(usize::from(pixel) % GROUP_BASES)
            .copied()
            .unwrap_or(0);
        let [r, g, b] = vidc1_shown(Rgb::from_riscos_word(base).to_array(), pixel);
        Rgb::new(r, g, b)
    })
}

/// A base colour word as entry `i` of its group shows it, its low byte kept.
const fn grouped(base: u32, i: u32) -> u32 {
    let [r, g, b] =
        tairix_vdu::palette::grouped(Rgb::from_riscos_word(base).to_array(), i.to_le_bytes()[0]);
    Rgb::new(r, g, b).riscos_word() | (base & 0xFF)
}

/// What `VDU 19,logical,physical,red,green,blue` sets: the kind of entry and
/// its two flash states, the state not set kept from `current`. `None` for a
/// physical colour no form names.
pub(crate) fn vdu_19(physical: u8, rgb: [u8; 3], current: [u32; 2]) -> Option<(u32, [u32; 2])> {
    let supreme = physical & 0x80 != 0;
    let given = word(Rgb::new(rgb[0], rgb[1], rgb[2]), supreme);
    match u32::from(physical & 0x7F) {
        p @ 0..=15 => {
            let [first, second] = PHYSICAL[p as usize];
            Some((kind::NORMAL, [word(first, supreme), word(second, supreme)]))
        }
        kind::NORMAL => Some((kind::NORMAL, [given; 2])),
        kind::FIRST => Some((kind::NORMAL, [given, current[1]])),
        kind::SECOND => Some((kind::NORMAL, [current[0], given])),
        kind::BORDER => Some((kind::BORDER, [given; 2])),
        kind::POINTER => Some((kind::POINTER, [given; 2])),
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
    let Some((how, pair)) = set else {
        return Ok(());
    };
    set_through(os, u32::from(logical), how, pair)
}

/// Logical colour `logical`, its entry `how`, set through `PaletteV` to the
/// flash states `first` and `second`.
pub(crate) fn set_through(
    os: &mut Os<'_>,
    logical: u32,
    how: u32,
    [first, second]: [u32; 2],
) -> Result<(), Fail> {
    let mut regs = Regs::default();
    regs.r[..5].copy_from_slice(&[logical, how, first, second, reason::SET]);
    os.call_vector(PALETTE_V, &mut regs, Entry::Supervisor)?;
    Ok(())
}

/// Logical colour `logical`'s two flash states, as `PaletteV` reads them.
pub(crate) fn read_through(os: &mut Os<'_>, logical: u32) -> Result<[u32; 2], Fail> {
    let mut regs = Regs::default();
    regs.r[..5].copy_from_slice(&[logical, kind::NORMAL, 0, 0, reason::READ]);
    os.call_vector(PALETTE_V, &mut regs, Entry::Supervisor)?;
    Ok([regs.r[2], regs.r[3]])
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
            let held = vdu
                .palette
                .read(&format, regs.r[0], regs.r[1])
                .unwrap_or([0, 0]);
            [regs.r[2], regs.r[3]] = reported(regs.r[1], held);
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
/// logical colours, or nought for those from nought up; R1 the count in its
/// low 24 bits and the kind in its top eight. A read gives the first and
/// second states' words at R2 and R3, either nought where not wanted. A
/// write takes its words from R2, as RISC OS 5 takes them: an entry's two
/// states in turn, or for the first or second flash state alone that
/// state's word.
fn block(os: &mut Os<'_>, regs: &mut Regs) -> Result<bool, Fail> {
    let (list, count, how) = (regs.r[0], regs.r[1] & 0xFF_FFFF, regs.r[1] >> 24);
    let write = regs.r[4] == reason::WRITE_BLOCK;
    for i in 0..count.min(len32(EIGHT_BIT_COLOURS)) {
        let logical = if list == 0 {
            i
        } else {
            os.read_word(list.wrapping_add(4 * i))?
        };
        if write {
            let words = regs.r[2];
            let pair = how == kind::NORMAL;
            let given = |os: &mut Os<'_>, k: u32| {
                os.read_word(words.wrapping_add(4 * (if pair { 2 * i } else { i } + k)))
            };
            let first = given(os, 0)?;
            let second = if pair { given(os, 1)? } else { first };
            with(os, |vdu, _| {
                let format = vdu.screen.format;
                let [old_first, old_second] = current(&vdu.palette, &format, logical, how);
                let set = match how {
                    kind::FIRST => [first, old_second],
                    kind::SECOND => [old_first, second],
                    _ => [first, second],
                };
                vdu.palette.write(&format, logical, how, set);
                Ok(())
            })?;
        } else {
            let (firsts, seconds) = (regs.r[2], regs.r[3]);
            let [first, second] = with(os, |vdu, _| {
                let format = vdu.screen.format;
                Ok(current(&vdu.palette, &format, logical, how))
            })?;
            if firsts != 0 {
                os.write_word(firsts.wrapping_add(4 * i), first)?;
            }
            if seconds != 0 {
                os.write_word(seconds.wrapping_add(4 * i), second)?;
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
                *entry = [word(rgb, false); 2];
            }
        }
        Ok(true)
    })
}

#[cfg(test)]
mod tests;
