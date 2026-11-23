//! `BlendTable` 0.02: the table that blends each colour of one palette over
//! each of another's at a translucency, as RISC OS 5's builds it. Each pair
//! is mixed a channel at a time in 256ths, then found in the destination: by
//! its inverse table where it has 256 colours, else by the sum of squared
//! channel differences, the last of equally near.

use alloc::vec::Vec;

use tairix_colour::Rgb;
use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::module::{Builtin, Resource, Swis, SWI_OUT_OF_RANGE};
use tairix_riscos_kernel::msgtrans::{Own, OwnFile};
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs, World};
use tairix_riscos_kernel::swi::chunk;

use crate::colourtrans::matching::Loadings;
use crate::colourtrans::readwrite::named_colours;
use crate::inverse::{self, fifteen};
use crate::palette::EIGHT_BIT_COLOURS;

/// Its SWIs, in their chunk's order.
const NAMES: &[&str] = &["GenerateTable", "UnlockTable"];

/// Its messages, and their file, kept at its workspace's start.
const MESSAGES: &[u8] = include_bytes!("../resources/BlendTable/Messages");
const MESSAGES_FILE: OwnFile = OwnFile::named(b"Resources:$.Resources.BlendTable.Messages\0");

/// A translucency past wholly transparent.
const BAD_TRANSLUCENCY: Error = Error {
    number: 0x94,
    token: "BadTranslucency",
};

/// `GenerateTable`'s flags.
mod flag {
    /// R2 points at the source sprite rather than naming it.
    pub(super) const SOURCE_POINTER: u32 = 1;
    /// R4 likewise for the destination.
    pub(super) const DESTINATION_POINTER: u32 = 1 << 1;
    /// The table kept in the module's own block until unlocked.
    pub(super) const LOCK: u32 = 1 << 2;
}

/// A blend's whole, in the 256ths it is measured in: the most translucent a
/// source is drawn, wholly transparent, its destination's colour alone.
pub(crate) const WHOLE: u32 = 256;

/// `BlendTable`.
pub(crate) static BLEND_TABLE: Builtin = Builtin {
    title: "BlendTable",
    help_name: None,
    bridge: false,
    swis: Some(Swis {
        chunk: chunk::BLEND_TABLE,
        prefix: "BlendTable",
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
    resources: &[Resource {
        name: "Resources.BlendTable.Messages",
        file_type: tairix_riscos_kernel::filing::filetype::TEXT,
        data: MESSAGES,
    }],
    code: None,
};

/// Its messages, and the tables its callers have locked.
struct State {
    own: Own,
    locked: Vec<u32>,
    /// The last 256-colour destination's inverse table, built again only
    /// for other colours.
    inverse: inverse::Kept,
}

fn own(world: &mut World) -> Option<&mut Own> {
    world.extension::<State>().map(|state| &mut state.own)
}

fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let workspace = os.rma_claim(MESSAGES_FILE.room())?;
    os.write_word(regs.r[12], workspace)?;
    let own = MESSAGES_FILE.place(os, workspace)?;
    os.hold_extension(State {
        own,
        locked: Vec::new(),
        inverse: inverse::Kept::default(),
    })
}

/// The tables still locked freed; its workspace the kernel frees from its
/// private word.
fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.close_own(own)?;
    if let Some(state) = os.release_extension::<State>() {
        for block in state.locked {
            os.free_rma_quietly(block);
        }
    }
    Ok(())
}

fn perform(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    match offset {
        0 => generate(os, regs),
        1 => unlock(os, regs),
        _ => Err(os.error_with(SWI_OUT_OF_RANGE, &[BLEND_TABLE.title.as_bytes()])),
    }
}

/// `BlendTable_GenerateTable`: the table for the source R1 and R2 name over
/// the destination R3 and R4 name at translucency R5, written to R6, or its
/// size alone where R6 is nought, or kept locked in a block of the module's
/// own given in R6; R0 and R1 the colours each has, R2 the table's size. A
/// translucency past 256, wholly transparent, is refused.
fn generate(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let flags = regs.r[0];
    let translucency = regs.r[5];
    if translucency > WHOLE {
        return Err(os.own_error(own, BAD_TRANSLUCENCY, &[]));
    }
    let source = named_colours(os, regs.r[1], regs.r[2], flags & flag::SOURCE_POINTER != 0)?;
    let destination = named_colours(
        os,
        regs.r[3],
        regs.r[4],
        flags & flag::DESTINATION_POINTER != 0,
    )?;
    let size = len32(source.len() * destination.len());
    regs.r[..3].copy_from_slice(&[len32(source.len()), len32(destination.len()), size]);
    let lock = flags & flag::LOCK != 0;
    if !lock && regs.r[6] == 0 {
        return Ok(());
    }
    let inverse = if destination.len() == EIGHT_BIT_COLOURS {
        let kept = os
            .extension::<State>()
            .and_then(|state| state.inverse.table(&destination));
        let Some((table, _)) = kept else {
            return Err(os.no_memory());
        };
        Some(table)
    } else {
        None
    };
    let Some(table) = blended(
        &source,
        &destination,
        inverse.as_deref().map(Vec::as_slice),
        translucency,
    ) else {
        return Err(os.no_memory());
    };
    if !lock {
        return os.write_bytes(regs.r[6], &table);
    }
    let recorded = os
        .extension::<State>()
        .is_some_and(|state| state.locked.try_reserve(1).is_ok());
    if !recorded {
        return Err(os.no_memory());
    }
    let at = os.rma_claim_with(&table)?;
    if let Some(state) = os.extension::<State>() {
        state.locked.push(at);
    }
    regs.r[6] = at;
    Ok(())
}

/// `BlendTable_UnlockTable`: the table locked at R0 given up.
fn unlock(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let block = regs.r[0];
    let held = os.extension::<State>().and_then(|state| {
        let at = state.locked.iter().position(|&locked| locked == block)?;
        Some(state.locked.swap_remove(at))
    });
    match held {
        Some(block) => os.rma_free(block),
        None => Ok(()),
    }
}

/// Each source colour blended over each destination colour, the
/// destination's colours outermost, a 256-colour destination's found
/// through its `inverse` table; none where there is no room.
fn blended(
    source: &[Rgb],
    destination: &[Rgb],
    inverse: Option<&[u8]>,
    translucency: u32,
) -> Option<Vec<u8>> {
    let mut table = Vec::new();
    table
        .try_reserve_exact(source.len() * destination.len())
        .ok()?;
    for &under in destination {
        for &over in source {
            let mixed = mix(over, under, translucency);
            table.push(match inverse {
                Some(inverse) => inverse[fifteen(mixed)],
                None => nearest_last(destination, mixed),
            });
        }
    }
    Some(table)
}

/// `over` drawn on `under` at `translucency` 256ths of the way to `under`.
pub(crate) fn mix(over: Rgb, under: Rgb, translucency: u32) -> Rgb {
    let [r, g, b] = core::array::from_fn(|c| {
        let (a, b) = (
            u32::from(over.to_array()[c]),
            u32::from(under.to_array()[c]),
        );
        u8::try_from((a * (WHOLE - translucency) + b * translucency) >> 8).unwrap_or(u8::MAX)
    });
    Rgb::new(r, g, b)
}

/// The colour number among `colours` nearest `wanted` by the sum of squared
/// channel differences, unweighted, the last of equally near.
pub(crate) fn nearest_last(colours: &[Rgb], wanted: Rgb) -> u8 {
    let mut best = (u32::MAX, 0);
    for (number, &colour) in (0..=u8::MAX).zip(colours) {
        let error = Loadings::UNIT.error(colour, wanted);
        if error <= best.0 {
            best = (error, number);
        }
    }
    best.1
}

#[cfg(test)]
mod tests;
