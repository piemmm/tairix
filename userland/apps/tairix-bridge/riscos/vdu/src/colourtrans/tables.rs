//! The tables ColourTrans builds to translate a source's colours to a
//! destination's, as RISC OS 5 builds them: `SelectTable`, `GenerateTable`
//! and `SelectGCOLTable`. A palettised source gives a byte for each of its
//! colours, or to a deep destination with the wide flag its pixel; a deep
//! source gives a palettised destination a 32K table; deep to deep needs
//! none. The source is a mode with a palette, or a sprite.

use alloc::vec::Vec;

use tairix_colour::Rgb;
use tairix_riscos_kernel::os::{len32, Entry, Fail, Os, Regs};

use super::matching::{default_256_match, Look};
use super::palettes::{self, Against};
use super::{current, loadings, screen_calibration, State, CURRENT};
use crate::colour::gcol_of_number;
use crate::inverse::GUARD;

/// `GenerateTable`'s flags, which `SelectTable` reads only for a sprite.
/// RISC OS 5 ignores bit 1, the current palette for a sprite with none.
mod flag {
    /// R1 points at the sprite rather than naming it.
    pub(super) const SPRITE_POINTER: u32 = 1;
    /// R6 and R7 give a transfer function for each source colour.
    pub(super) const TRANSFER: u32 = 1 << 2;
    /// A deep destination is given its pixels, two or four bytes each.
    pub(super) const WIDE: u32 = 1 << 4;
    /// The table's form, in the top byte.
    pub(super) const FORM_SHIFT: u32 = 24;
}

/// The table's form: colour numbers, or the source's palette.
const PHYSICAL_PALETTE: u32 = 1;
/// The 32K tables kept at once.
const KEPT_32K: usize = 2;

/// A 32K table built and where its RMA block is: for the colours it was
/// built against.
pub(crate) struct ThirtyTwoK {
    pub(crate) at: u32,
    key: Vec<u32>,
}

/// `ColourTrans_SelectTable`, R5 read only for a sprite, and
/// `GenerateTable`, R5 always read: R4 nought for the table's size in R4.
pub(super) fn select_table(os: &mut Os<'_>, regs: &mut Regs, generate: bool) -> Result<(), Fail> {
    let sprite = palettes::names_sprite(os, regs.r[0])?;
    let flags = if generate || sprite { regs.r[5] } else { 0 };
    let Some(loadings) = loadings(os) else {
        return Ok(());
    };
    let source = source_colours(os, regs.r[0], regs.r[1], sprite.then_some(flags))?;
    let source = match source {
        Some(colours) if flags & flag::TRANSFER != 0 => {
            Some(transferred(os, &colours, regs.r[6], regs.r[7])?)
        }
        other => other,
    };
    let destination = destination(os, regs.r[2], regs.r[3])?;
    let table = match (source, destination.palette()) {
        (Some(colours), _) if flags >> flag::FORM_SHIFT == PHYSICAL_PALETTE => colours
            .iter()
            .flat_map(|&c| c.riscos_word().to_le_bytes())
            .collect(),
        (Some(colours), Some(_)) => colours
            .iter()
            .map(|&c| {
                destination
                    .colour_number(c, loadings, Look::Nearest)
                    .to_le_bytes()[0]
            })
            .collect(),
        (Some(colours), None) if flags & flag::WIDE != 0 => {
            let width = destination.pixel_bytes();
            colours
                .iter()
                .flat_map(|&c| {
                    let pixel = destination.colour_number(c, loadings, Look::Nearest);
                    pixel.to_le_bytes().into_iter().take(width)
                })
                .collect()
        }
        (Some(colours), None) => colours
            .iter()
            .map(|&c| gcol_of_number(default_256_match(c, loadings, Look::Nearest)))
            .collect(),
        (None, Some(_)) => {
            let at = thirty_two_k(os, &destination)?;
            [GUARD, at, GUARD]
                .iter()
                .flat_map(|w| w.to_le_bytes())
                .collect()
        }
        (None, None) => Vec::new(),
    };
    give(os, regs, &table)
}

/// `ColourTrans_SelectGCOLTable`: each source colour's GCOL in the
/// destination, at R4.
pub(super) fn select_gcol_table(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let sprite = palettes::names_sprite(os, regs.r[0])?.then_some(regs.r[5]);
    let Some(loadings) = loadings(os) else {
        return Ok(());
    };
    let Some(colours) = source_colours(os, regs.r[0], regs.r[1], sprite)? else {
        return Ok(());
    };
    let destination = destination(os, regs.r[2], regs.r[3])?;
    let table: Vec<u8> = colours
        .iter()
        .map(|&c| destination.gcol(c, loadings, Look::Nearest).to_le_bytes()[0])
        .collect();
    os.write_bytes(regs.r[4], &table)
}

/// The table written at R4, or its size given in R4 where R4 is nought.
fn give(os: &mut Os<'_>, regs: &mut Regs, table: &[u8]) -> Result<(), Fail> {
    if regs.r[4] == 0 {
        regs.r[4] = len32(table.len());
        return Ok(());
    }
    os.write_bytes(regs.r[4], table)
}

/// The destination R2 and R3 name: the current mode's colours as last read
/// for both -1, else the mode's with the palette R3 names, matched through
/// the calibration.
fn destination(os: &mut Os<'_>, mode: u32, palette: u32) -> Result<Against, Fail> {
    if (mode, palette) == (CURRENT, CURRENT) {
        if let Some(against) = current(os)? {
            return Ok(against.clone());
        }
    }
    let named = palettes::named(os, mode, palette)?;
    Ok(named.calibrated_by(screen_calibration(os)))
}

/// A source's colours in colour-number order, none for a deep one: a mode
/// with its palette, or the sprite R0 names where `sprite` gives its flags.
fn source_colours(
    os: &mut Os<'_>,
    r0: u32,
    r1: u32,
    sprite: Option<u32>,
) -> Result<Option<Vec<Rgb>>, Fail> {
    let against = match sprite {
        Some(flags) => sprite_colours(os, r0, r1, flags)?,
        None if (r0, r1) == (CURRENT, CURRENT) => {
            return Ok(current(os)?.and_then(palette_colours));
        }
        None => palettes::source(os, r0, r1)?,
    };
    Ok(palette_colours(&against))
}

/// The colours of a palettised `against`; none for direct colour.
fn palette_colours(against: &Against) -> Option<Vec<Rgb>> {
    against.palette().map(|(_, colours)| colours.to_vec())
}

/// A sprite's colours: its palette's, or without one its mode's default.
fn sprite_colours(os: &mut Os<'_>, r0: u32, r1: u32, flags: u32) -> Result<Against, Fail> {
    let sprite = palettes::sprite_of(os, r0, r1, flags & flag::SPRITE_POINTER != 0)?;
    if sprite.named_entries() == 0 {
        return palettes::named(os, sprite.mode, 0);
    }
    let words: Vec<u32> = palettes::sprite_pairs(os, &sprite)?
        .iter()
        .map(|&[first, _]| first)
        .collect();
    palettes::of_mode_words(os, sprite.mode, &words)
}

/// Each colour passed through the transfer function at `routine`, R12
/// `workspace`.
fn transferred(
    os: &mut Os<'_>,
    colours: &[Rgb],
    workspace: u32,
    routine: u32,
) -> Result<Vec<Rgb>, Fail> {
    let bits = os.caller_bits();
    let mut out = Vec::new();
    if out.try_reserve_exact(colours.len()).is_err() {
        return Err(os.no_memory());
    }
    for &colour in colours {
        let mut regs = Regs::default();
        regs.r[0] = colour.riscos_word();
        regs.r[12] = workspace;
        os.call_routine(routine, bits, Entry::Supervisor, &mut regs)?;
        out.push(Rgb::from_riscos_word(regs.r[0]));
    }
    Ok(out)
}

/// The 32K table for `destination`: kept from an earlier call where one
/// matches, else built into a block of its own.
fn thirty_two_k(os: &mut Os<'_>, destination: &Against) -> Result<u32, Fail> {
    let colours = destination
        .palette()
        .map_or(&[][..], |(_, colours)| colours);
    let key: Vec<u32> = colours.iter().map(|c| c.riscos_word()).collect();
    if let Some(at) = os
        .extension::<State>()
        .and_then(|state| state.tables.iter().find(|t| t.key == key).map(|t| t.at))
    {
        return Ok(at);
    }
    let Some(table) = destination.inverse() else {
        return Err(os.no_memory());
    };
    let at = os.rma_claim_with(&table)?;
    let evicted = os.extension::<State>().and_then(|state| {
        state.tables.push(ThirtyTwoK { at, key });
        (state.tables.len() > KEPT_32K).then(|| state.tables.remove(0))
    });
    if let Some(old) = evicted {
        os.rma_free(old.at)?;
    }
    Ok(at)
}
