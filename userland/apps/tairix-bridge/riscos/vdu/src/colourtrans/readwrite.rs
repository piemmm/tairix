//! `ReadPalette` and `WritePalette`: a palette's words as held, each entry's
//! two flash states where asked. The palette read is the current mode's,
//! the sprite's output goes to where it has one, a mode's default, a block
//! of the caller's words, or a sprite's.

use alloc::vec::Vec;

use tairix_colour::Rgb;
use tairix_image::SPRITE_HEADER_LEN;
use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::os::{len32, Entry, Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::SPRITE_OP;
use tairix_riscos_kernel::swi::X;

use super::palettes::{self, Holding};
use super::{own, CURRENT};
use crate::palette::{kind, reason, PALETTE_V};
use crate::sprite::{Naming, Sprite};

/// The flags both take.
mod flag {
    /// R1 points at the sprite rather than naming it.
    pub(super) const SPRITE_POINTER: u32 = 1;
    /// Each entry's two flash states, the first first.
    pub(super) const FLASHING: u32 = 1 << 1;
}

/// A buffer too small for the palette asked for.
const BUFFER_TOO_SMALL: Error = Error {
    number: 0xA06,
    token: "BufferSmall",
};

/// The current palette written while output goes to a sprite.
const SWITCHED: Error = Error {
    number: 0xA03,
    token: "Switched",
};

/// `OS_SpriteOp` 37 making a palette: its R3, and its top bit asking an
/// eight-bit sprite's to hold 256 entries.
const MAKE_PALETTE: u32 = 37;
const NEW_PALETTE: u32 = 1;
const FULL_PALETTE: u32 = 1 << 31;

/// A depth of display with no palette.
pub(super) const NO_PALETTE: Error = Error {
    number: 0xA07,
    token: "NoPalette",
};

/// `ColourTrans_ReadPalette`: the palette R0 and R1 name written to R2's
/// buffer of R3 bytes, R2 and R3 left past it; R2 nought for its size in R3.
pub(super) fn read_palette(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let by_pointer = regs.r[4] & flag::SPRITE_POINTER != 0;
    let pairs = named_pairs(os, regs.r[0], regs.r[1], by_pointer)?;
    let flashing = regs.r[4] & flag::FLASHING != 0;
    let words = if flashing { 2 } else { 1 };
    let size = len32(4 * words * pairs.len());
    if regs.r[2] == 0 {
        regs.r[3] = size;
        return Ok(());
    }
    if regs.r[3] < size {
        return Err(os.own_error(own, BUFFER_TOO_SMALL, &[]));
    }
    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(size as usize).is_err() {
        return Err(os.no_memory());
    }
    for pair in &pairs {
        for word in &pair[..words] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
    }
    os.write_bytes(regs.r[2], &bytes)?;
    regs.r[2] = regs.r[2].wrapping_add(size);
    regs.r[3] -= size;
    Ok(())
}

/// The palette R0 and R1 name, each entry's two states, as `ReadPalette`
/// reads them: a sprite's in R0's area, by pointer where `by_pointer`, else
/// mode R0's, -1 the current one's, with palette R1: -1 the current
/// palette, nought the mode's default, else a block of words.
pub(crate) fn named_pairs(
    os: &mut Os<'_>,
    r0: u32,
    r1: u32,
    by_pointer: bool,
) -> Result<Vec<[u32; 2]>, Fail> {
    if r0 != CURRENT && palettes::names_sprite(os, r0)? {
        let sprite = palettes::sprite_of(os, r0, r1, by_pointer)?;
        if sprite.named_entries() != 0 {
            return palettes::sprite_pairs(os, &sprite);
        }
        let holding = palettes::holding_of(os, sprite.mode)?;
        return Ok(palettes::default_pairs(with_palette(os, holding)?));
    }
    let holding = palettes::holding_of(os, r0)?;
    let holding = with_palette(os, holding)?;
    match r1 {
        CURRENT => palettes::current_pairs(os, holding.colours()),
        0 => Ok(palettes::default_pairs(holding)),
        block => {
            let mut pairs = Vec::new();
            if pairs.try_reserve_exact(holding.colours()).is_err() {
                return Err(os.no_memory());
            }
            for i in 0..len32(holding.colours()) {
                pairs.push([os.read_word(block.wrapping_add(4 * i))?; 2]);
            }
            Ok(pairs)
        }
    }
}

/// The colours of the palette R0 and R1 name, each entry's first state, as
/// [`named_pairs`] reads them.
pub(crate) fn named_colours(
    os: &mut Os<'_>,
    r0: u32,
    r1: u32,
    by_pointer: bool,
) -> Result<Vec<Rgb>, Fail> {
    let pairs = named_pairs(os, r0, r1, by_pointer)?;
    Ok(pairs
        .iter()
        .map(|&[first, _]| Rgb::from_riscos_word(first))
        .collect())
}

/// `ColourTrans_WritePalette`: R2's words made the palette R0 and R1 name,
/// every register kept: the current palette, refused while output goes to
/// a sprite, or a sprite's, given a palette of its mode's size where it has
/// fewer. The words are kept as given, and ColourTrans's own reading of the
/// current palette is not made stale, as RISC OS 5 leaves both.
pub(super) fn write_palette(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let flashing = regs.r[4] & flag::FLASHING != 0;
    if regs.r[0] == CURRENT {
        if palettes::output_switched(os) {
            return Err(os.own_error(own, SWITCHED, &[]));
        }
        let holding = palettes::holding_of(os, CURRENT)?;
        let count = len32(with_palette(os, holding)?.colours());
        return write_current(os, count, regs.r[2], flashing);
    }
    let by_pointer = regs.r[4] & flag::SPRITE_POINTER != 0;
    let sprite = palettes::sprite_of(os, regs.r[0], regs.r[1], by_pointer)?;
    let holding = palettes::holding_of(os, sprite.mode)?;
    let count = with_palette(os, holding)?.colours();
    let sprite = if (sprite.palette_entries() as usize) < count {
        let full = if matches!(holding, Holding::Grouped | Holding::Full) {
            FULL_PALETTE
        } else {
            0
        };
        let make = MAKE_PALETTE | Naming::Pointer.value();
        let mut call = Regs::new([
            make,
            regs.r[0],
            sprite.at,
            NEW_PALETTE | full,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
        os.perform(X | SPRITE_OP, &mut call)?;
        Sprite::read(os, sprite.at)?
    } else {
        sprite
    };
    let pairs = given_pairs(os, regs.r[2], count, flashing)?;
    os.write_bytes(sprite.address(SPRITE_HEADER_LEN), &pairs)
}

/// `count` entries' two states from `table`, as bytes: the table's own
/// pairs where `flashing`, else each word for both.
fn given_pairs(os: &mut Os<'_>, table: u32, count: usize, flashing: bool) -> Result<Vec<u8>, Fail> {
    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(8 * count).is_err() {
        return Err(os.no_memory());
    }
    let words = if flashing { 2 * count } else { count };
    for i in 0..len32(words) {
        let word = os.read_word(table.wrapping_add(4 * i))?.to_le_bytes();
        bytes.extend_from_slice(&word);
        if !flashing {
            bytes.extend_from_slice(&word);
        }
    }
    Ok(bytes)
}

/// The current palette's `count` entries set through `PaletteV` from
/// `table`: pairs where `flashing`, else each word for both states.
fn write_current(os: &mut Os<'_>, count: u32, table: u32, flashing: bool) -> Result<(), Fail> {
    let call = |os: &mut Os<'_>, pairs: u32| -> Result<(), Fail> {
        let mut regs = Regs::new([
            0,
            count | (kind::NORMAL << 24),
            pairs,
            0,
            reason::WRITE_BLOCK,
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
    };
    if flashing {
        return call(os, table);
    }
    let pairs = given_pairs(os, table, count as usize, false)?;
    os.below_supervisor_stack(len32(pairs.len()), |os, at| {
        os.write_bytes(at, &pairs)?;
        call(os, at)
    })
}

/// `holding`, refused where it holds no palette.
fn with_palette(os: &mut Os<'_>, holding: Holding) -> Result<Holding, Fail> {
    if holding.colours() == 0 {
        return Err(os.own_error(own, NO_PALETTE, &[]));
    }
    Ok(holding)
}
