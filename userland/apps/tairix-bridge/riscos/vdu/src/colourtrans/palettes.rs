//! What a ColourTrans call matches against: a mode, the current one or one
//! a number, selector or mode word names, and its colours, the current
//! palette's, the mode's default or a block of the caller's words.

use alloc::rc::Rc;
use alloc::vec::Vec;

use tairix_colour::Rgb;
use tairix_image::{SPRITE_HEADER_LEN, SPRITE_PALETTE_ENTRY_LEN};
use tairix_riscos_kernel::os::{len32, Entry, Fail, Os, Regs};
use tairix_vdu::mode::{var, Flags};

use super::calibration::Calibration;
use super::matching::{self, Loadings, Look};
use super::CURRENT;
use crate::colour::{gcol_of_number, Direct};
use crate::palette::{
    default_words, grouped_colours, kind, reason, EIGHT_BIT_COLOURS, GROUP_BASES, PALETTE_V,
};
use crate::screen::Named;
use crate::sprite::{Area, Naming, Sprite, UNKNOWN_MODE};

/// How a mode holds its colours.
#[derive(Clone, Copy)]
pub(crate) enum Holding {
    /// A palette of two, four or sixteen entries, at this log2 of bits a
    /// pixel.
    Palette(u32),
    /// 256 colours made from sixteen entries, a pixel's top four bits its
    /// own.
    Grouped,
    /// 256 entries of its own.
    Full,
    /// No palette: each pixel its colour.
    Direct(Direct),
}

impl Holding {
    const fn of(log2_bpp: u32, ncolour: u32, flags: u32) -> Self {
        match log2_bpp {
            0..=2 => Self::Palette(log2_bpp),
            3 if Flags::new(flags).has(Flags::FULL_PALETTE) => Self::Full,
            3 => Self::Grouped,
            _ => Self::Direct(Direct::of_parts(log2_bpp, ncolour, flags)),
        }
    }

    /// The colours it shows: its palette's entries, a 256-colour mode's
    /// 256; none for a direct-colour mode.
    pub(crate) const fn colours(self) -> usize {
        match self {
            Self::Palette(log2_bpp) => 1 << (1 << log2_bpp),
            Self::Grouped | Self::Full => EIGHT_BIT_COLOURS,
            Self::Direct(_) => 0,
        }
    }

    /// The words a palette of it holds.
    const fn entries(self) -> usize {
        match self {
            Self::Palette(log2_bpp) => 1 << (1 << log2_bpp),
            Self::Grouped => GROUP_BASES,
            Self::Full => EIGHT_BIT_COLOURS,
            Self::Direct(_) => 0,
        }
    }

    /// The log2 of the bits of a pixel its palette colours; none for a
    /// direct-colour mode.
    const fn log2_bpp(self) -> Option<u32> {
        match self {
            Self::Palette(log2_bpp) => Some(log2_bpp),
            Self::Grouped | Self::Full => Some(3),
            Self::Direct(_) => None,
        }
    }

    /// Whether its colour numbers and GCOLs differ: a 256-colour mode's
    /// grouped palette, not a full one.
    const fn shuffled(self) -> bool {
        matches!(self, Self::Grouped)
    }
}

/// The colours a match is made against.
#[derive(Clone)]
pub(crate) struct Against {
    holding: Holding,
    /// The colours in colour-number order: a grouped mode's 256.
    colours: [Rgb; EIGHT_BIT_COLOURS],
    len: usize,
    /// They read as the default 256-colour palette.
    default_256: bool,
    /// The calibration a wanted colour is shown through before a palettised
    /// match, as RISC OS 5 applies it.
    calibration: Option<Rc<Calibration>>,
}

impl Against {
    /// The colours of `words`, palette words of `holding`.
    pub(crate) fn of_words(holding: Holding, words: &[u32]) -> Self {
        let mut colours = [Rgb::BLACK; EIGHT_BIT_COLOURS];
        let len = if matches!(holding, Holding::Grouped) {
            for (colour, shown) in colours.iter_mut().zip(grouped_colours(words)) {
                *colour = shown;
            }
            EIGHT_BIT_COLOURS
        } else {
            for (colour, &word) in colours.iter_mut().zip(words) {
                *colour = Rgb::from_riscos_word(word);
            }
            words.len().min(EIGHT_BIT_COLOURS)
        };
        let default_256 = matches!(holding, Holding::Grouped | Holding::Full)
            && matching::is_default_256(&colours[..len]);
        Self {
            holding,
            colours,
            len,
            default_256,
            calibration: None,
        }
    }

    /// These colours matched with a wanted colour shown through
    /// `calibration`.
    pub(crate) fn calibrated_by(self, calibration: Option<Rc<Calibration>>) -> Self {
        Self {
            calibration,
            ..self
        }
    }

    /// The colour a palettised match looks for in place of `wanted`.
    pub(crate) fn shown(&self, wanted: Rgb) -> Rgb {
        self.calibration
            .as_ref()
            .map_or(wanted, |calibration| calibration.convert(wanted))
    }

    /// The colour number matching `wanted`.
    pub(crate) fn colour_number(&self, wanted: Rgb, loadings: Loadings, look: Look) -> u32 {
        match self.holding {
            Holding::Direct(direct) => matching::direct_match(direct, wanted, look),
            _ if self.default_256 => u32::from(matching::default_256_match(
                self.shown(wanted),
                loadings,
                look,
            )),
            _ => {
                let at = matching::search(
                    &self.colours[..self.len],
                    self.shown(wanted),
                    loadings,
                    look,
                );
                u32::try_from(at).unwrap_or(0)
            }
        }
    }

    /// The 32K table of a palettised mode, as RISC OS 5 builds it; none for
    /// a direct-colour mode, or where there is no room for it.
    pub(crate) fn inverse(&self) -> Option<Vec<u8>> {
        self.palette()?;
        crate::inverse::table(&self.colours[..self.len])
    }

    /// The bytes a pixel takes: one for a palettised mode.
    pub(crate) const fn pixel_bytes(&self) -> usize {
        match self.holding {
            Holding::Direct(direct) => direct.bytes(),
            Holding::Palette(_) | Holding::Grouped | Holding::Full => 1,
        }
    }

    /// A palettised mode's depth and colours, in colour-number order; none
    /// for a direct-colour mode.
    pub(crate) fn palette(&self) -> Option<(u32, &[Rgb])> {
        Some((self.holding.log2_bpp()?, &self.colours[..self.len]))
    }

    /// The GCOL matching `wanted`: a grouped 256-colour mode's colour number
    /// with its bits as `GCOL` takes them.
    pub(crate) fn gcol(&self, wanted: Rgb, loadings: Loadings, look: Look) -> u32 {
        self.gcol_of(self.colour_number(wanted, loadings, look))
    }

    /// Colour number `number`'s GCOL, as [`Self::gcol`] gives it.
    pub(crate) fn gcol_of(&self, number: u32) -> u32 {
        if self.holding.shuffled() {
            u32::from(gcol_of_number(number.to_le_bytes()[0]))
        } else {
            number
        }
    }

    /// The GCOL `ColourTrans_SetGCOL` reports for colour number `number`: a
    /// 256-colour mode's GCOL form, its palette grouped or full.
    pub(crate) fn reported_gcol(&self, number: u32) -> u32 {
        match self.holding {
            Holding::Grouped | Holding::Full => u32::from(gcol_of_number(number.to_le_bytes()[0])),
            Holding::Palette(_) | Holding::Direct(_) => number,
        }
    }
}

/// How mode `mode` holds its colours, `u32::MAX` the current one as
/// ColourTrans takes it; "no such screen mode" where it reads as none.
pub(crate) fn holding_of(os: &mut Os<'_>, mode: u32) -> Result<Holding, Fail> {
    let mode = if mode == CURRENT {
        current_mode(os)?
    } else {
        mode
    };
    let wanted = [var::LOG2_BPP, var::N_COLOUR, var::MODE_FLAGS];
    match crate::vars::mode_variables(os, mode, wanted)? {
        [Some(log2_bpp), Some(ncolour), Some(flags)] => Ok(Holding::of(log2_bpp, ncolour, flags)),
        _ => Err(os.error(UNKNOWN_MODE)),
    }
}

/// Mode `mode`'s colours with `words`, a sprite's palette: a 256-colour
/// mode's whole where there are 256 of them. Entries it lacks are black.
pub(crate) fn of_mode_words(os: &mut Os<'_>, mode: u32, words: &[u32]) -> Result<Against, Fail> {
    let holding = match holding_of(os, mode)? {
        Holding::Grouped if words.len() >= Holding::Full.entries() => Holding::Full,
        holding => holding,
    };
    let mut held = [0u32; EIGHT_BIT_COLOURS];
    let entries = holding.entries();
    for (slot, &word) in held[..entries].iter_mut().zip(words) {
        *slot = word;
    }
    Ok(Against::of_words(holding, &held[..entries]))
}

/// The current mode's colours as its palette holds them now, each first
/// flash state's.
pub(crate) fn current(os: &mut Os<'_>) -> Result<Against, Fail> {
    let holding = holding_of(os, CURRENT)?;
    let pairs = current_pairs(os, holding.entries())?;
    let words: Vec<u32> = pairs.iter().map(|&[first, _]| first).collect();
    Ok(Against::of_words(holding, &words))
}

/// The current palette as held, `count` entries' two states each: that of
/// the sprite output goes to where the sprite has a palette of its own, as
/// RISC OS 5's ColourTrans takes it, else the screen's through `PaletteV`.
pub(crate) fn current_pairs(os: &mut Os<'_>, count: usize) -> Result<Vec<[u32; 2]>, Fail> {
    if let Some(sprite) = redirected_sprite(os)? {
        let mut pairs = sprite_pairs(os, &sprite)?;
        pairs.truncate(count);
        return Ok(pairs);
    }
    let count32 = u32::try_from(count).unwrap_or(0);
    let bytes = 8 * count32;
    let mut pairs = Vec::new();
    if pairs.try_reserve_exact(count).is_err() {
        return Err(os.no_memory());
    }
    os.below_supervisor_stack(bytes, |os, at| {
        let seconds = at + 4 * count32;
        let mut regs = Regs::new([
            0,
            count32 | (kind::NORMAL << 24),
            at,
            seconds,
            reason::READ_BLOCK,
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
        for i in 0..count32 {
            pairs.push([os.read_word(at + 4 * i)?, os.read_word(seconds + 4 * i)?]);
        }
        Ok(())
    })?;
    Ok(pairs)
}

/// A source naming the sprite at R1 outside any area.
const BARE_SPRITE: u32 = 256;

/// Whether R0 names a sprite: R1 itself where it is 256, else in R0's area,
/// told from a mode selector by its first word, whose bottom bit a
/// selector's has set.
pub(crate) fn names_sprite(os: &mut Os<'_>, r0: u32) -> Result<bool, Fail> {
    if r0 == BARE_SPRITE {
        return Ok(true);
    }
    Ok(r0 > BARE_SPRITE && r0.is_multiple_of(4) && os.read_word(r0)? & 1 == 0)
}

/// The sprite R0 and R1 name: R1's where R0 is 256, else in R0's area by
/// name, or by pointer where `by_pointer`.
pub(crate) fn sprite_of(
    os: &mut Os<'_>,
    r0: u32,
    r1: u32,
    by_pointer: bool,
) -> Result<Sprite, Fail> {
    if r0 == BARE_SPRITE {
        return Sprite::read(os, r1);
    }
    let naming = if by_pointer {
        Naming::Pointer
    } else {
        Naming::Named
    };
    Area::named(os, naming, r0)?.sprite(os, naming, r1)
}

/// Whether output goes to a sprite, its image or its mask.
pub(crate) fn output_switched(os: &mut Os<'_>) -> bool {
    crate::state::read(os, |vdu, _| vdu.context.0[2] != 0).unwrap_or(false)
}

/// The sprite output goes to, where it has a palette of its own.
fn redirected_sprite(os: &mut Os<'_>) -> Result<Option<Sprite>, Fail> {
    Ok(output_sprite(os)?.filter(|sprite| sprite.palette_entries() != 0))
}

/// The sprite output goes to, if it goes to a sprite's image.
fn output_sprite(os: &mut Os<'_>) -> Result<Option<Sprite>, Fail> {
    let Some([switched, _, at, _]) = crate::state::read(os, |vdu, _| vdu.context.0) else {
        return Ok(None);
    };
    if at == 0 || switched & 0xFF != crate::sprite::reason::SWITCH_TO_SPRITE {
        return Ok(None);
    }
    Sprite::read(os, at).map(Some)
}

/// The colours the output's destination shows: the current ones, save that a
/// sprite with no palette of its own shows its mode's default.
pub(crate) fn destination_colours(os: &mut Os<'_>) -> Result<Against, Fail> {
    match output_sprite(os)? {
        Some(sprite) if sprite.palette_entries() == 0 => named(os, sprite.mode, 0),
        _ => current(os),
    }
}

/// The mode ColourTrans takes as current: the screen's while output goes to
/// a sprite with no palette of its own, as RISC OS 5's does, else the
/// destination's, `u32::MAX`.
fn current_mode(os: &mut Os<'_>) -> Result<u32, Fail> {
    match output_sprite(os)? {
        Some(sprite) if sprite.palette_entries() == 0 => crate::state::with(os, |vdu, _| {
            Ok(match vdu.screen.named {
                Named::Number(number) => number,
                Named::Selector(address) => address,
            })
        }),
        _ => Ok(CURRENT),
    }
}

/// `sprite`'s palette, each entry's two states as held, or as RISC OS 5
/// shows an eight-bit sprite's palette of fewer than 256: its first
/// sixteen's colours made 256 as VIDC1 would, bare of their low bytes.
pub(crate) fn sprite_pairs(os: &mut Os<'_>, sprite: &Sprite) -> Result<Vec<[u32; 2]>, Fail> {
    let entries = sprite.named_entries();
    let mut pairs = Vec::new();
    if pairs.try_reserve_exact(Holding::Full.colours()).is_err() {
        return Err(os.no_memory());
    }
    for i in 0..entries {
        let at = sprite.address(SPRITE_HEADER_LEN + i * SPRITE_PALETTE_ENTRY_LEN);
        pairs.push([os.read_word(at)?, os.read_word(at.wrapping_add(4))?]);
    }
    let short = entries < len32(Holding::Full.colours());
    if short && matches!(holding_of(os, sprite.mode)?, Holding::Grouped) {
        let bases: Vec<u32> = pairs
            .iter()
            .take(Holding::Grouped.entries())
            .map(|&[first, _]| first)
            .collect();
        pairs.clear();
        pairs.extend(grouped_colours(&bases).map(|shown| [shown.riscos_word(); 2]));
    }
    Ok(pairs)
}

/// The default palette of a mode held as `holding`, each entry steady in
/// its first state's colour.
pub(crate) fn default_pairs(holding: Holding) -> Vec<[u32; 2]> {
    default_firsts(holding)
        .take(holding.colours())
        .map(|first| [first; 2])
        .collect()
}

/// The first flash states of the default palette of a mode held as
/// `holding`, in colour-number order; none for a direct-colour mode.
fn default_firsts(holding: Holding) -> impl Iterator<Item = u32> {
    holding
        .log2_bpp()
        .map(|log2_bpp| default_words(log2_bpp, false))
        .into_iter()
        .flatten()
        .map(|[first, _]| first)
}

/// Mode `mode`'s colours with palette `palette`: nought the mode's
/// default, else a block of its words. The current palette, -1, is the
/// current mode's alone; a named mode given it takes its default.
pub(crate) fn named(os: &mut Os<'_>, mode: u32, palette: u32) -> Result<Against, Fail> {
    let holding = holding_of(os, mode)?;
    of_holding(os, holding, palette)
}

/// A table's source, mode `mode` with palette `palette`, as [`named`] takes
/// it, but for a 256-colour mode of sixteen entries, whose default stands
/// whatever palette is given, as RISC OS 5 has it.
pub(crate) fn source(os: &mut Os<'_>, mode: u32, palette: u32) -> Result<Against, Fail> {
    let holding = holding_of(os, mode)?;
    let palette = if matches!(holding, Holding::Grouped) {
        0
    } else {
        palette
    };
    of_holding(os, holding, palette)
}

/// Colours held as `holding` with palette `palette`: nought or -1 its
/// default, else a block of its words.
fn of_holding(os: &mut Os<'_>, holding: Holding, palette: u32) -> Result<Against, Fail> {
    let entries = holding.entries();
    let mut words = [0u32; EIGHT_BIT_COLOURS];
    if palette == 0 || palette == u32::MAX {
        for (word, first) in words[..entries].iter_mut().zip(default_firsts(holding)) {
            *word = first;
        }
    } else {
        for (i, word) in (0u32..).zip(&mut words[..entries]) {
            *word = os.read_word(palette.wrapping_add(4 * i))?;
        }
    }
    Ok(Against::of_words(holding, &words[..entries]))
}

#[cfg(test)]
mod tests;
