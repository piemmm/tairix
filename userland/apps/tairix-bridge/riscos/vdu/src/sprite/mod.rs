//! Sprites (`plans/RISCOS-EMULATOR.md` §8): `OS_SpriteOp` through
//! `SpriteV`, whose default is the kernel's reasons here and, while it is
//! active, SpriteExtend's in front of them; sprite areas and their sprites in
//! guest memory, the system area being dynamic area 3. Every rule is held to
//! genuine RISC OS 5.30 by the recorded corpus; where RISC OS 5 reads past an
//! area it is given or faults, the call is refused with the error its own
//! verification gives.

pub(crate) mod alpha;
mod area;
mod create;
mod dump;
mod edit;
pub(crate) mod extend;
mod file;
mod format;
mod get;
mod header;
mod join;
mod jpeg;
mod output;
mod paint;
mod pixels;
mod system;
pub(crate) mod utils;
mod verify;

use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};
use tairix_riscos_kernel::swi::sprite_op;

use crate::canvas::Block;
use crate::palette::EIGHT_BIT_COLOURS;
use crate::plot::Ink;
use crate::state;

pub(crate) use format::{variable as mode_variable, UNKNOWN_MODE};
pub(crate) use paint::Palettised;
pub(crate) use system::{
    area_handler as system_area_handler, make_area as make_system_area, selected_size,
    size as system_size, Sprites,
};

pub(crate) use area::{Area, Naming};
pub(crate) use header::Sprite;

/// `OS_SpriteOp`'s reasons, R0's low byte.
pub(crate) mod reason {
    pub(crate) const SCREEN_SAVE: u32 = 2;
    pub(crate) const SCREEN_LOAD: u32 = 3;
    pub(crate) const READ_AREA: u32 = 8;
    pub(crate) const INITIALISE: u32 = 9;
    pub(crate) const LOAD: u32 = 10;
    pub(crate) const MERGE: u32 = 11;
    pub(crate) const SAVE: u32 = 12;
    pub(crate) const NAME: u32 = 13;
    pub(crate) const GET: u32 = 14;
    pub(crate) const CREATE: u32 = 15;
    pub(crate) const GET_AT: u32 = 16;
    pub(crate) const VERIFY: u32 = 17;
    pub(crate) const SELECT: u32 = 24;
    pub(crate) const DELETE: u32 = 25;
    pub(crate) const RENAME: u32 = 26;
    pub(crate) const COPY: u32 = 27;
    pub(crate) const PUT: u32 = 28;
    pub(crate) const CREATE_MASK: u32 = 29;
    pub(crate) const REMOVE_MASK: u32 = 30;
    pub(crate) const INSERT_ROW: u32 = 31;
    pub(crate) const DELETE_ROW: u32 = 32;
    pub(crate) const FLIP_X: u32 = 33;
    pub(crate) const PUT_AT: u32 = 34;
    pub(crate) const APPEND: u32 = 35;
    pub(crate) const POINTER: u32 = 36;
    pub(crate) const PALETTE: u32 = 37;
    /// Reason [`PALETTE`]'s R3 bit asking an eight-bit sprite's new palette
    /// to hold 256 entries.
    pub(crate) const PALETTE_FULL: u32 = 1 << 31;
    pub(crate) const ALPHA: u32 = 38;
    pub(crate) const INFO: u32 = super::sprite_op::READ_INFO;
    pub(crate) const READ_PIXEL: u32 = 41;
    pub(crate) const WRITE_PIXEL: u32 = 42;
    pub(crate) const READ_MASK: u32 = 43;
    pub(crate) const WRITE_MASK: u32 = 44;
    pub(crate) const INSERT_COLUMN: u32 = 45;
    pub(crate) const DELETE_COLUMN: u32 = 46;
    pub(crate) const FLIP_Y: u32 = 47;
    pub(crate) const PLOT_MASK: u32 = 48;
    pub(crate) const PLOT_MASK_AT: u32 = 49;
    pub(crate) const PLOT_MASK_SCALED: u32 = 50;
    pub(crate) const PAINT_CHARACTER: u32 = 51;
    pub(crate) const PUT_SCALED: u32 = 52;
    pub(crate) const PUT_GREY_SCALED: u32 = 53;
    pub(crate) const REMOVE_WASTAGE: u32 = 54;
    pub(crate) const PLOT_MASK_TRANSFORMED: u32 = 55;
    pub(crate) const PUT_TRANSFORMED: u32 = super::sprite_op::PUT_TRANSFORMED;
    pub(crate) const ROWS: u32 = 57;
    pub(crate) const COLUMNS: u32 = 58;
    pub(crate) const SWITCH_TO_SPRITE: u32 = 60;
    pub(crate) const SWITCH_TO_MASK: u32 = super::sprite_op::SWITCH_TO_MASK;
    pub(crate) const SAVE_AREA_SIZE: u32 = 62;
    pub(crate) const TILE: u32 = 65;
}

/// The entries of an eight-bit palette that makes its pixels plain colour
/// numbers, each its own: the full palette.
pub(crate) const FULL_PALETTE: u32 = len32(EIGHT_BIT_COLOURS);

/// "No system sprite area": dynamic area 3 holds nothing.
pub(crate) const NO_SYSTEM_AREA: Error = Error {
    number: 0x80,
    token: "NoSystemSprites",
};
/// "No room in the sprite area" for a new sprite.
pub(crate) const NO_ROOM: Error = Error {
    number: 0x82,
    token: "NoRoomForSprite",
};
/// "No sprites to save": an area holding none saved.
pub(crate) const NO_SPRITES: Error = Error {
    number: 0x83,
    token: "NoSpritesToSave",
};
/// "Not enough memory for the sprite operation".
pub(crate) const NO_MEMORY: Error = Error {
    number: 0x85,
    token: "SpriteNoMemory",
};
/// "Sprite not found".
pub(crate) const NOT_FOUND: Error = Error {
    number: 0x86,
    token: "SpriteNotFound",
};
/// "Not a sprite file".
pub(crate) const BAD_FILE: Error = Error {
    number: 0x700,
    token: "BadSpriteFile",
};
/// "No room to copy the sprite".
pub(crate) const NO_ROOM_TO_COPY: Error = Error {
    number: 0x701,
    token: "NoRoomToCopySprite",
};
/// "Outside the sprite": a pixel, row or column it does not have.
pub(crate) const OUTSIDE: Error = Error {
    number: 0x703,
    token: "OutsideSprite",
};
/// "No room to enlarge the sprite".
pub(crate) const NO_ROOM_TO_GROW: Error = Error {
    number: 0x706,
    token: "NoRoomToGrowSprite",
};
/// "A translation table is needed to plot this sprite": its depth differs
/// from the destination's, or a table cannot reach the destination's.
pub(crate) const TABLE_REQUIRED: Error = Error {
    number: 0x70B,
    token: "SpriteTableRequired",
};
/// "`OS_SpriteOp` has no such reason".
pub(crate) const UNKNOWN_REASON: Error = Error {
    number: 0x709,
    token: "UnknownSpriteOp",
};
/// "Sprites of different shapes or modes cannot be joined".
pub(crate) const CANNOT_JOIN: Error = Error {
    number: 0x70E,
    token: "CannotJoinSprites",
};
/// "A reserved flag bit is set": an alpha conversion or a JPEG call asked
/// for something its flags do not define.
pub(crate) const RESERVED_FLAGS: Error = Error {
    number: 0x712,
    token: "SpriteReservedFlags",
};
/// "A sprite of this depth has no palette": a palette asked of a sprite of
/// more than 256 colours.
pub(crate) const NO_DEEP_PALETTE: Error = Error {
    number: 0x716,
    token: "NoDeepSpritePalette",
};
/// "No palette for this depth": a new sprite of more than 256 colours asked
/// for one.
pub(crate) const NO_DEEP_NEW_PALETTE: Error = Error {
    number: 0x718,
    token: "NoDeepNewSpritePalette",
};
/// "Unrecognised sprite data": an area or sprite whose blocks do not hold
/// together.
pub(crate) const UNRECOGNISED: Error = Error {
    number: 0x71E,
    token: "UnrecognisedSpriteData",
};

/// `VDU 23,27`: action 0 selects the system area's sprite named `number` in
/// decimal, as `*SChoose` does, and 1 takes it from the screen between the
/// last two graphics points, as `*SGet` does; any other does nothing. An
/// error goes nowhere, the VDU stream having nowhere to give it.
pub(crate) fn vdu_23_27(os: &mut Os<'_>, action: u8, number: u8) -> Result<(), Fail> {
    let operation = match action {
        0 => reason::SELECT,
        1 => reason::GET,
        _ => return Ok(()),
    };
    let name = create::numbered_name(number);
    let mut spelt = [0u8; 4];
    spelt[..name.as_bytes().len()].copy_from_slice(name.as_bytes());
    let at = os.scratch_text(&spelt)?;
    let mut regs = Regs::new([operation, 0, at, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    match os.perform(
        tairix_riscos_kernel::swi::X | tairix_riscos_kernel::swi::os::SPRITE_OP,
        &mut regs,
    ) {
        Ok(()) | Err(Fail::Error(_)) => Ok(()),
        Err(unwind) => Err(unwind),
    }
}

/// `PLOT` &E8 to &EF: the system area's selected sprite at the graphics
/// cursor, by the foreground GCOL action; or its shape, as `OS_SpriteOp` 48
/// plots it, in the background colour or inverting what is there. Nothing
/// where none is selected. An error goes nowhere, as `VDU 23,27`'s does.
pub(crate) fn plot_selected(os: &mut Os<'_>, ink: Ink) -> Result<(), Fail> {
    match plot_selected_sprite(os, ink) {
        Ok(()) | Err(Fail::Error(_)) => Ok(()),
        Err(unwind) => Err(unwind),
    }
}

fn plot_selected_sprite(os: &mut Os<'_>, ink: Ink) -> Result<(), Fail> {
    let Some(name) = state::read(os, |vdu, _| vdu.sprites.selected).flatten() else {
        return Ok(());
    };
    let system = Area::system(os)?;
    let Some(sprite) = system.find(os, &name)? else {
        return Ok(());
    };
    let at = paint::cursor(os)?;
    match ink {
        Ink::Foreground => {
            let action = state::read(os, |vdu, _| vdu.colours.fore.action).unwrap_or(0);
            paint::put(os, &sprite, at, action & SPRITE_ACTION)
        }
        Ink::Background => paint::plot_mask(os, &sprite, at),
        Ink::Inverse => paint::plot_shape(os, &sprite, at, Block::solid(0, u32::MAX)),
    }
}

/// A GCOL action's bits a sprite plot takes: the operation and the mask.
const SPRITE_ACTION: u32 = 0xF;

/// `SpriteV`'s default: the kernel's reasons.
pub(crate) fn vector(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    kernel(os, regs)
}

/// The kernel's reasons; any other is unknown, after its sprite is looked up
/// where it names one.
pub(crate) fn kernel(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let Some(naming) = Naming::of(regs.r[0]) else {
        return Err(os.error(UNKNOWN_REASON));
    };
    match regs.r[0] & sprite_op::REASON {
        reason::SCREEN_SAVE => dump::save(os, regs),
        reason::SCREEN_LOAD => dump::load(os, regs),
        reason::READ_AREA => create::read_area(os, naming, regs),
        reason::INITIALISE => create::initialise(os, naming, regs),
        reason::LOAD => file::load(os, naming, regs, false),
        reason::MERGE => file::load(os, naming, regs, true),
        reason::SAVE => file::save(os, naming, regs),
        reason::NAME => create::name(os, naming, regs),
        reason::GET => get::from_cursors(os, naming, regs),
        reason::CREATE => create::create(os, naming, regs),
        reason::GET_AT => get::from_rectangle(os, naming, regs),
        reason::SWITCH_TO_SPRITE | reason::SWITCH_TO_MASK => output::switch(os, naming, regs),
        reason::SAVE_AREA_SIZE => output::save_area_size(os, naming, regs),
        // R1 is the character, not an area.
        reason::PAINT_CHARACTER => {
            let at = paint::at(os, regs.r[3], regs.r[4])?;
            paint::paint_character(os, regs.r[1].to_le_bytes()[0], at, regs.r[6])
        }
        // The reasons naming a sprite in R2, which the kernel looks up before
        // it knows whether it has the reason.
        sprite_reason @ reason::SELECT..=reason::SAVE_AREA_SIZE => {
            let area = Area::named(os, naming, regs.r[1])?;
            let sprite = area.sprite(os, naming, regs.r[2])?;
            let target = create::Target {
                area,
                naming,
                sprite,
            };
            match sprite_reason {
                reason::SELECT => {
                    create::select(os, &target, regs);
                    Ok(())
                }
                reason::DELETE => create::delete(os, &target),
                reason::RENAME => create::rename(os, &target, regs),
                reason::COPY => create::copy(os, &target, regs),
                reason::PUT => {
                    let at = paint::cursor(os)?;
                    paint::put(os, &target.sprite, at, regs.r[5])
                }
                reason::PUT_AT => {
                    let at = paint::at(os, regs.r[3], regs.r[4])?;
                    paint::put(os, &target.sprite, at, regs.r[5])
                }
                reason::PLOT_MASK => {
                    let at = paint::cursor(os)?;
                    paint::plot_mask(os, &target.sprite, at)
                }
                reason::PLOT_MASK_AT => {
                    let at = paint::at(os, regs.r[3], regs.r[4])?;
                    paint::plot_mask(os, &target.sprite, at)
                }
                reason::CREATE_MASK => edit::create_mask(os, &target),
                reason::REMOVE_MASK => edit::remove_mask(os, &target),
                reason::FLIP_X => edit::flip_rows(os, &target),
                reason::INFO => pixels::info(os, &target, regs),
                reason::READ_PIXEL => pixels::read_pixel(os, &target, regs),
                reason::WRITE_PIXEL => pixels::write_pixel(os, &target, regs),
                reason::READ_MASK => pixels::read_mask(os, &target, regs),
                reason::WRITE_MASK => pixels::write_mask(os, &target, regs),
                reason::FLIP_Y => edit::flip_columns(os, &target),
                reason::REMOVE_WASTAGE => edit::remove_wastage(os, &target),
                _ => Err(os.error(UNKNOWN_REASON)),
            }
        }
        _ => Err(os.error(UNKNOWN_REASON)),
    }
}

#[cfg(test)]
mod tests;
