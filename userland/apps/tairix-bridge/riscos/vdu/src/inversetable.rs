//! `InverseTable` 0.18: a 256-colour palette's colours in fifteen-bit form and
//! its 32K inverse table, for the current destination and for any palette or
//! sprite, as RISC OS 5's builds them. The current destination's are kept,
//! and made again only once its palette differs.

use alloc::vec::Vec;

use tairix_colour::Rgb;
use tairix_riscos_kernel::module::{Builtin, Swis, SWI_OUT_OF_RANGE};
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};
use tairix_riscos_kernel::swi::chunk;

use crate::colourtrans::palettes;
use crate::colourtrans::readwrite::named_colours;
use crate::colourtrans::CURRENT;
use crate::inverse::{self, fifteen, CELLS};
use crate::palette::EIGHT_BIT_COLOURS;
use crate::sprite::UNKNOWN_MODE;

/// Its SWIs, in their chunk's order.
const NAMES: &[&str] = &["Calculate", "SpriteTable"];

/// `InverseTable`.
pub(crate) static INVERSE_TABLE: Builtin = Builtin {
    title: "InverseTable",
    help_name: None,
    bridge: false,
    swis: Some(Swis {
        chunk: chunk::INVERSE_TABLE,
        prefix: "InverseTable",
        names: NAMES,
        handler: perform,
    }),
    services: None,
    commands: &[],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[],
    vectors: &[],
    resources: &[],
    code: None,
};

/// The bytes of a colour table: a word a colour.
const COLOURS_BYTES: u32 = len32(4 * EIGHT_BIT_COLOURS);

/// The block the current destination's tables are kept in, its workspace,
/// the colour table first, and the colours they were made for.
struct State {
    tables: u32,
    kept: inverse::Kept,
}

fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let tables = os.rma_claim(COLOURS_BYTES + len32(CELLS))?;
    os.write_word(regs.r[12], tables)?;
    os.hold_extension(State {
        tables,
        kept: inverse::Kept::default(),
    })
}

#[expect(clippy::unnecessary_wraps, reason = "a module entry's signature")]
fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.release_extension::<State>();
    Ok(())
}

fn perform(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    match offset {
        0 => calculate(os, regs),
        1 => sprite_table(os, regs),
        _ => Err(os.error_with(SWI_OUT_OF_RANGE, &[INVERSE_TABLE.title.as_bytes()])),
    }
}

/// `InverseTable_Calculate`: R0 the current destination's colour table, R1
/// its inverse table, made again where its palette changed.
fn calculate(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let colours = colours_of(os, CURRENT, CURRENT, false)?;
    let Some(state) = os.extension::<State>() else {
        return Ok(());
    };
    let blocks = (state.tables, state.tables + COLOURS_BYTES);
    let Some((inverse, built)) = state.kept.table(&colours) else {
        return Err(os.no_memory());
    };
    if built {
        if let Err(fail) = write_tables(os, &colours, &inverse, blocks) {
            if let Some(state) = os.extension::<State>() {
                state.kept.forget();
            }
            return Err(fail);
        }
    }
    [regs.r[0], regs.r[1]] = [blocks.0, blocks.1];
    Ok(())
}

/// `InverseTable_SpriteTable`: the tables for the palette R2 and R3 name,
/// a mode's or a sprite's in R2's area, written to R0's and R1's blocks;
/// for the current mode and palette, both -1, the current destination's
/// tables as `Calculate` gives them, the blocks left alone.
fn sprite_table(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if (regs.r[2], regs.r[3]) == (CURRENT, CURRENT) {
        return calculate(os, regs);
    }
    let colours = colours_of(os, regs.r[2], regs.r[3], true)?;
    let Some(inverse) = inverse::table(&colours) else {
        return Err(os.no_memory());
    };
    write_tables(os, &colours, &inverse, (regs.r[0], regs.r[1]))
}

/// The 256 colours the palette `r0` and `r1` name, as `ReadPalette` takes
/// them; "no such screen mode" where it is not a 256-colour palette.
fn colours_of(os: &mut Os<'_>, r0: u32, r1: u32, by_pointer: bool) -> Result<Vec<Rgb>, Fail> {
    let mode = if r0 != CURRENT && palettes::names_sprite(os, r0)? {
        palettes::sprite_of(os, r0, r1, by_pointer)?.mode
    } else {
        r0
    };
    if palettes::holding_of(os, mode)?.colours() != EIGHT_BIT_COLOURS {
        return Err(os.error(UNKNOWN_MODE));
    }
    named_colours(os, r0, r1, by_pointer)
}

/// `colours`' fifteen-bit forms and their inverse table written to the
/// blocks `(colours, inverse)`.
fn write_tables(
    os: &mut Os<'_>,
    colours: &[Rgb],
    inverse: &[u8],
    (to_colours, to_inverse): (u32, u32),
) -> Result<(), Fail> {
    let words: Vec<u8> = colours
        .iter()
        .flat_map(|&colour| u32::try_from(fifteen(colour)).unwrap_or(0).to_le_bytes())
        .collect();
    os.write_bytes(to_colours, &words)?;
    os.write_bytes(to_inverse, inverse)
}
