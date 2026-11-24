//! The colours fonts are painted in, as RISC OS 5.30's Font Manager keeps
//! them. A paint starts from a background and a foreground logical colour
//! and an anti-aliasing offset, and paints each of its sixteen levels of
//! coverage in a table's pixel: in a mode of up to sixteen colours the
//! logical colours themselves, the offset's thresholds stepping the levels
//! from the background up the foreground's run; in a deeper mode the pseudo
//! palette's entry for the foreground, which `Font_SetPalette` fills with
//! the colour numbers of the shades between its two colours.

use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::font::TRUE_COLOURS;
use tairix_riscos_kernel::swi::{colour_trans, X};
use tairix_riscos_vdu::render::{self, WHOLE};

use crate::module::{error, raise, with, State};

/// The levels of coverage a glyph is painted in, from none to whole.
pub(crate) const LEVELS: usize = WHOLE as usize + 1;

/// A pixel for each level.
pub(crate) type Table = [u32; LEVELS];

/// The colours a paint starts with: the font, its background and foreground
/// logical colours, how many colours past the foreground anti-aliasing
/// reaches, negative counting down, and the RGB colours last set, where
/// they were set as RGB: background and foreground as `&BBGGRR00`, and the
/// most anti-aliasing offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Colours {
    pub(crate) font: u32,
    pub(crate) background: u32,
    pub(crate) foreground: u32,
    pub(crate) offset: i32,
    pub(crate) rgb: Option<(u32, u32, i32)>,
}

impl Colours {
    /// The colours at start, font 1 among them though no font is found yet.
    pub(crate) const INITIAL: Self = Self {
        font: 1,
        background: 0,
        foreground: 1,
        offset: MOST_OFFSET,
        rgb: None,
    };
}

/// The furthest anti-aliasing reaches from the foreground either way.
pub(crate) const MOST_OFFSET: i32 = 14;

/// Each offset's thresholds at start: where each anti-aliasing level steps
/// up to the next colour. Offset 0's one threshold can be set.
const THRESHOLDS: [&[u8]; 15] = [
    &[5],
    &[4, 9],
    &[4, 8, 12],
    &[3, 6, 9, 12],
    &[3, 5, 8, 10, 13],
    &[3, 5, 8, 10, 12, 14],
    &[2, 4, 6, 8, 10, 12, 14],
    &[2, 4, 6, 8, 9, 11, 13, 15],
    &[2, 4, 5, 7, 8, 10, 11, 13, 14],
    &[2, 4, 5, 7, 8, 10, 11, 13, 14, 15],
    &[2, 4, 5, 7, 8, 9, 11, 12, 13, 14, 15],
    &[2, 3, 4, 6, 7, 8, 10, 11, 12, 13, 14, 15],
    &[2, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 15],
    &[2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
];
/// What ends a threshold table.
const TABLE_END: u8 = 0xFF;
/// The longest table: its offset, fifteen thresholds and its end.
const TABLE_MOST: usize = 17;

/// The deepest mode whose pixels are logical colours: sixteen colours.
const LOGICAL_MOST: u32 = 2;
/// The foreground colours the pseudo palette has an entry for.
const ENTRIES: usize = 16;
/// A colour's channels' top four bits.
const NIBBLES: u32 = 0xF0F0_F000;

/// A deeper mode's pixels for a foreground colour, matched at the depth
/// `log2_bpp`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    log2_bpp: u32,
    table: Table,
}

/// The colours the Font Manager keeps beside a paint's: offset 0's
/// threshold, which `Font_SetThresholds` sets, and the pseudo palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Palette {
    pub(crate) threshold: u8,
    entries: [Option<Entry>; ENTRIES],
    /// The table `Font_SetColourTable` gave a direct-colour mode, which its
    /// paints take until colours are set again.
    explicit: Option<Entry>,
}

impl Palette {
    pub(crate) const INITIAL: Self = Self {
        threshold: THRESHOLDS[0][0],
        entries: [None; ENTRIES],
        explicit: None,
    };

    /// The table a paint of `colours` takes at `log2_bpp`: the explicit
    /// one where it was given at that depth, else theirs.
    pub(crate) fn paint_table(&self, colours: &Colours, log2_bpp: u32) -> Result<Table, Unpainted> {
        match &self.explicit {
            Some(entry) if entry.log2_bpp == log2_bpp => Ok(entry.table),
            _ => self.table(colours, log2_bpp),
        }
    }

    /// The thresholds offset `offset` paints with.
    pub(crate) fn thresholds(&self, offset: i32) -> impl Iterator<Item = u8> + '_ {
        let n = usize::try_from(offset.unsigned_abs()).unwrap_or(usize::MAX);
        let table = THRESHOLDS.get(n).copied().unwrap_or(&[]);
        let settable = (n == 0).then_some(self.threshold);
        settable
            .into_iter()
            .chain(table.iter().copied().skip(usize::from(n == 0)))
    }

    /// The shade each level paints in under offset `offset`: nought the
    /// background, then each of the foreground's run in turn.
    fn shades(&self, offset: i32) -> [u32; LEVELS] {
        core::array::from_fn(|level| {
            let level = u8::try_from(level).unwrap_or(WHOLE);
            let passed = self.thresholds(offset).filter(|&t| t <= level).count();
            u32::try_from(passed).unwrap_or(u32::MAX)
        })
    }

    /// The table `colours` paint in on a surface of `log2_bpp` bits a
    /// pixel: in a mode of sixteen colours or fewer refused where the
    /// background, the foreground or the run from it leaves the mode's
    /// colours, in a deeper one where the pseudo palette holds nothing for
    /// the foreground. An entry matched at another depth paints nought.
    pub(crate) fn table(&self, colours: &Colours, log2_bpp: u32) -> Result<Table, Unpainted> {
        if log2_bpp <= LOGICAL_MOST {
            let count = 1i64 << (1u32 << log2_bpp);
            let last = i64::from(colours.foreground) + i64::from(colours.offset);
            let within = |colour: i64| (0..count).contains(&colour);
            return if within(colours.background.into())
                && within(colours.foreground.into())
                && within(last)
            {
                Ok(self.logical(colours))
            } else {
                Err(Unpainted::Invalid)
            };
        }
        let entry = entry_of(colours.foreground)
            .and_then(|at| self.entries[at].as_ref())
            .ok_or(Unpainted::Undefined)?;
        Ok(if entry.log2_bpp == log2_bpp {
            entry.table
        } else {
            [0; LEVELS]
        })
    }

    /// The logical colours each level paints in.
    fn logical(&self, colours: &Colours) -> Table {
        let step = if colours.offset < 0 { -1 } else { 1 };
        self.shades(colours.offset).map(|shade| match shade {
            0 => colours.background,
            shade => {
                let along = step * i32::try_from(shade - 1).unwrap_or(0);
                colours.foreground.wrapping_add_signed(along)
            }
        })
    }
}

/// Why the current colours paint in no table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unpainted {
    /// A colour is not one of the mode's.
    Invalid,
    /// The pseudo palette holds nothing for the foreground.
    Undefined,
}

/// The pseudo palette's entry for foreground `foreground`; none past its
/// sixteen.
fn entry_of(foreground: u32) -> Option<usize> {
    usize::try_from(foreground).ok().filter(|&at| at < ENTRIES)
}

/// The shade `step` of `steps` from `from` to `to`, words `&BBGGRR00`: each
/// channel stepped by its difference over `steps` in 16.16, rounded down,
/// each multiple rounded down again, as RISC OS 5.30 steps them; the ends
/// the two colours themselves.
fn shade(from: u32, to: u32, step: u32, steps: u32) -> u32 {
    if step == 0 || steps == 0 {
        return from;
    }
    if step >= steps {
        return to;
    }
    let [_, fr, fg, fb] = from.to_le_bytes();
    let [_, tr, tg, tb] = to.to_le_bytes();
    let channel = |a: u8, b: u8| {
        let difference = (i64::from(b) - i64::from(a)) << 16;
        let stride = difference.div_euclid(i64::from(steps));
        let moved = (i64::from(step) * stride).div_euclid(1 << 16);
        u8::try_from((i64::from(a) + moved).clamp(0, 255)).unwrap_or(u8::MAX)
    };
    u32::from_le_bytes([0, channel(fr, tr), channel(fg, tg), channel(fb, tb)])
}

/// A colour as `Font_SetPalette` takes it without its magic word: each
/// channel's top four bits repeated below them.
const fn nibbled(colour: u32) -> u32 {
    (colour & NIBBLES) | ((colour & NIBBLES) >> 4)
}

/// `Font_CurrentFont`, or `Font_FutureFont` where `future`: the offset as
/// the byte RISC OS 5.30 keeps it in.
pub(crate) fn current_font(os: &mut Os<'_>, regs: &mut Regs, future: bool) -> Result<(), Fail> {
    let colours = with(
        os,
        |state| if future { state.future } else { state.current },
    )?;
    regs.r[..4].copy_from_slice(&[
        colours.font,
        colours.background,
        colours.foreground,
        u32::from(colours.offset.to_le_bytes()[0]),
    ]);
    Ok(())
}

/// `Font_CurrentRGB`, or `Font_FutureRGB` where `future`.
pub(crate) fn rgb(os: &mut Os<'_>, regs: &mut Regs, future: bool) -> Result<(), Fail> {
    let colours = with(
        os,
        |state| if future { state.future } else { state.current },
    )?;
    let Some((background, foreground, most)) = colours.rgb else {
        return Err(raise(os, error::NO_RGB, &[]));
    };
    regs.r[..4].copy_from_slice(&[colours.font, background, foreground, most.cast_unsigned()]);
    Ok(())
}

/// The offset in R3 of a colour setting call; "bad colours" past 14 either
/// way.
fn offset_of(os: &mut Os<'_>, regs: &Regs) -> Result<i32, Fail> {
    let offset = regs.r[3].cast_signed();
    if offset.unsigned_abs() > MOST_OFFSET.unsigned_abs() {
        return Err(raise(os, error::BAD_COLOURS, &[]));
    }
    Ok(offset)
}

/// `Font_SetFontColours`: R0 the font, nought for the current one, then the
/// background, the foreground and the offset, at most 14 either way.
pub(crate) fn set_font_colours(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let offset = offset_of(os, regs)?;
    let font = regs.r[0];
    if font != 0 {
        crate::find::check_range(os, font)?;
    }
    with(os, |state| {
        if font != 0 {
            state.current.font = font;
        }
        state.choose(low_byte(regs.r[1]), low_byte(regs.r[2]), offset, false);
    })
}

/// A colour number as the Font Manager keeps one: its low byte.
fn low_byte(given: u32) -> u32 {
    u32::from(given.to_le_bytes()[0])
}

/// `Font_SetPalette`: the colours set as `Font_SetFontColours` sets them,
/// and R4 and R5, the background's and the last foreground's colours, laid
/// between them. In a mode of up to sixteen colours each logical colour of
/// the run is set to its shade; in a deeper one the shades' colour numbers
/// fill the pseudo palette's entry for the foreground, and a foreground
/// with no entry is refused once the colours are set.
pub(crate) fn set_palette(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let offset = offset_of(os, regs)?;
    let (background, foreground) = (low_byte(regs.r[1]), low_byte(regs.r[2]));
    let [from, to] = if regs.r[6] == TRUE_COLOURS {
        [regs.r[4], regs.r[5]]
    } else {
        [nibbled(regs.r[4]), nibbled(regs.r[5])]
    };
    let steps = offset.unsigned_abs() + 1;
    let log2_bpp = render::surface(os).map_or(0, |s| s.log2_bpp);
    if log2_bpp <= LOGICAL_MOST {
        render::set_logical_colour(os, background, from)?;
        let step = if offset < 0 { -1 } else { 1 };
        for k in 0..steps {
            let logical = foreground.wrapping_add_signed(step * k.cast_signed());
            render::set_logical_colour(os, logical, shade(from, to, k + 1, steps))?;
        }
        return with(os, |state| {
            state.choose(background, foreground, offset, true);
        });
    }
    let Some(at) = entry_of(foreground) else {
        with(os, |state| {
            state.choose(background, foreground, offset.abs(), true);
        })?;
        return Err(raise(os, error::UNDEFINED_COLOUR, &[]));
    };
    let shades = with(os, |state| state.palette.shades(offset))?;
    let mut numbers = [None; LEVELS];
    let mut table = [0; LEVELS];
    for (&k, slot) in shades.iter().zip(table.iter_mut()) {
        let held = &mut numbers[usize::try_from(k).unwrap_or(0).min(LEVELS - 1)];
        *slot = if let Some(number) = *held {
            number
        } else {
            let number = colour_number(os, shade(from, to, k, steps))?;
            *held = Some(number);
            number
        };
    }
    with(os, |state| {
        state.palette.entries[at] = Some(Entry { log2_bpp, table });
        state.choose(background, foreground, offset.abs(), true);
    })
}

/// `Font_SetColourTable`, ColourTrans's own call: R0 the font, nought for
/// the current one, R1 sixteen pixel words a direct-colour mode's paints
/// take, nought for none, R2 and R3 the background's and foreground's RGB
/// words and R4 the most offset asked for, which `Font_CurrentRGB` gives.
pub(crate) fn set_colour_table(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let font = regs.r[0];
    if font != 0 {
        crate::find::check_range(os, font)?;
    }
    let surface = render::surface(os);
    let log2_bpp = surface.map_or(0, |s| s.log2_bpp);
    let explicit = if regs.r[1] != 0 && surface.is_some_and(|s| s.direct) {
        let mut table = [0; LEVELS];
        for (i, slot) in (0u32..).zip(table.iter_mut()) {
            *slot = os.read_word(regs.r[1].wrapping_add(4 * i))?;
        }
        Some(Entry { log2_bpp, table })
    } else {
        None
    };
    with(os, |state| {
        if font != 0 {
            state.current.font = font;
        }
        state.current.rgb = Some((regs.r[2], regs.r[3], regs.r[4].cast_signed()));
        if explicit.is_some() {
            state.palette.explicit = explicit;
        }
    })
}

impl State {
    /// Colours chosen by number: the background, the foreground and the
    /// offset made current and an explicit table given up; the RGB colours
    /// too unless `keeping_rgb`, as `Font_SetPalette` keeps them.
    pub(crate) fn choose(
        &mut self,
        background: u32,
        foreground: u32,
        offset: i32,
        keeping_rgb: bool,
    ) {
        self.current.background = background;
        self.current.foreground = foreground;
        self.current.offset = offset;
        if !keeping_rgb {
            self.current.rgb = None;
        }
        self.palette.explicit = None;
    }
}

/// `colours`, as a string changed them, made current: an RGB change taken
/// through `ColourTrans_SetFontColours`, as RISC OS 5.30 takes one, and the
/// colours that sets; a change of number giving an explicit table up.
pub(crate) fn string_changed(os: &mut Os<'_>, colours: &mut Colours) -> Result<(), Fail> {
    if let Some((background, foreground, most)) = colours.rgb {
        let mut call = Regs::default();
        call.r[..4].copy_from_slice(&[colours.font, background, foreground, most.cast_unsigned()]);
        os.perform(X | colour_trans::SET_FONT_COLOURS, &mut call)?;
        let set = with(os, |state| state.current)?;
        *colours = Colours {
            font: colours.font,
            rgb: Some((background, foreground, most)),
            ..set
        };
    }
    let now = *colours;
    with(os, |state| {
        if now.rgb.is_none() {
            state.palette.explicit = None;
        }
        state.current = now;
    })
}

/// The colour number ColourTrans matches `colour` to.
fn colour_number(os: &mut Os<'_>, colour: u32) -> Result<u32, Fail> {
    let mut call = Regs::default();
    call.r[0] = colour;
    os.perform(X | colour_trans::RETURN_COLOUR_NUMBER, &mut call)?;
    Ok(call.r[0])
}

/// The table the current colours paint in, on the surface output goes to
/// now: "invalid font colour" where a colour is not the mode's, "undefined
/// font colour" where the pseudo palette holds nothing for the foreground.
pub(crate) fn current_table(os: &mut Os<'_>, colours: &Colours) -> Result<Table, Fail> {
    let log2_bpp = render::surface(os).map_or(0, |s| s.log2_bpp);
    let table = with(os, |state| state.palette.table(colours, log2_bpp))?;
    unpainted(os, table)
}

/// The table a paint of `colours` takes: as [`current_table`], save that
/// a table `Font_SetColourTable` gave a direct-colour mode stands while it
/// does.
pub(crate) fn paint_table(os: &mut Os<'_>, colours: &Colours) -> Result<Table, Fail> {
    let log2_bpp = render::surface(os).map_or(0, |s| s.log2_bpp);
    let table = with(os, |state| state.palette.paint_table(colours, log2_bpp))?;
    unpainted(os, table)
}

/// A table, or the error its absence gives.
fn unpainted(os: &mut Os<'_>, table: Result<Table, Unpainted>) -> Result<Table, Fail> {
    match table {
        Ok(table) => Ok(table),
        Err(Unpainted::Invalid) => Err(raise(os, error::INVALID_COLOUR, &[])),
        Err(Unpainted::Undefined) => Err(raise(os, error::UNDEFINED_COLOUR, &[])),
    }
}

/// `Font_ReadColourTable`: the sixteen pixels the current colours paint
/// in, a byte each, at R1.
pub(crate) fn read_colour_table(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let current = with(os, |state| state.current)?;
    let table = current_table(os, &current)?;
    let bytes: [u8; LEVELS] = core::array::from_fn(|i| table[i].to_le_bytes()[0]);
    os.write_bytes(regs.r[1], &bytes)
}

/// `Font_ReadThresholds`: the current offset's table at R1.
pub(crate) fn read_thresholds(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (table, length) = with(os, |state| {
        let offset = state.current.offset;
        let mut table = [TABLE_END; TABLE_MOST];
        table[0] = offset.to_le_bytes()[0];
        let mut length = 1;
        for (slot, threshold) in table[1..].iter_mut().zip(state.palette.thresholds(offset)) {
            *slot = threshold;
            length += 1;
        }
        (table, length + 1)
    })?;
    os.write_bytes(regs.r[1], &table[..length])
}

/// `Font_SetThresholds`: R1's table, as RISC OS 5.30 takes one — its offset
/// made current, and its first threshold made offset 0's, whatever offset
/// the table is for.
pub(crate) fn set_thresholds(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let offset = i32::from(os.read_byte(regs.r[1])?.cast_signed());
    if offset.unsigned_abs() > MOST_OFFSET.unsigned_abs() {
        return Err(raise(os, error::BAD_COLOURS, &[]));
    }
    let first = os.read_byte(regs.r[1].wrapping_add(1))?;
    with(os, |state| {
        state.current.offset = offset;
        state.palette.threshold = first;
    })
}

#[cfg(test)]
mod tests;
