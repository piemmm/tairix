//! SpriteUtils: the `*` commands on the system sprite area, each the
//! `OS_SpriteOp` it names on that area, its error the call's. `*SDelete`
//! stops at the first sprite it cannot delete. `*SList` and `*SInfo` print
//! the names, and the area's size, free space and count, in the emulator's
//! own words.

use alloc::vec::Vec;

use tairix_riscos_kernel::command_parameters;
use tairix_riscos_kernel::module::{Builtin, Command};
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::SPRITE_OP;
use tairix_riscos_kernel::swi::X;

use super::area::{Area, Naming};
use super::reason;

/// The most bytes of a command's tail read.
const TAIL_MOST: usize = 1024;

/// SpriteUtils.
pub(crate) static SPRITE_UTILS: Builtin = Builtin {
    title: "SpriteUtils",
    help_name: None,
    bridge: false,
    swis: None,
    services: None,
    commands: &[
        Command::new(
            "SChoose",
            (1, 1),
            "Selects a sprite of the system area for PLOT's sprite plots.",
            Some("Syntax: *SChoose <name>"),
            choose,
        ),
        Command::new(
            "SCopy",
            (2, 2),
            "Copies a sprite of the system area under a new name.",
            Some("Syntax: *SCopy <name> <new name>"),
            copy,
        ),
        Command::new(
            "ScreenLoad",
            (1, 1),
            "Plots a sprite file's first sprite at the graphics window's corner.",
            Some("Syntax: *ScreenLoad <file>"),
            screen_load,
        ),
        Command::new(
            "ScreenSave",
            (1, 1),
            "Saves the graphics window and the palette as a sprite file.",
            Some("Syntax: *ScreenSave <file>"),
            screen_save,
        ),
        Command::new(
            "SDelete",
            (1, 255),
            "Deletes sprites from the system area.",
            Some("Syntax: *SDelete <name> [<name> ...]"),
            delete,
        ),
        Command::new(
            "SFlipX",
            (1, 1),
            "Turns a sprite of the system area upside down.",
            Some("Syntax: *SFlipX <name>"),
            flip_x,
        ),
        Command::new(
            "SFlipY",
            (1, 1),
            "Turns a sprite of the system area left to right.",
            Some("Syntax: *SFlipY <name>"),
            flip_y,
        ),
        Command::new(
            "SGet",
            (1, 1),
            "Takes a sprite into the system area from between the last two graphics points.",
            Some("Syntax: *SGet <name>"),
            get,
        ),
        Command::new(
            "SInfo",
            (0, 0),
            "Gives the system sprite area's size, free space and sprites.",
            Some("Syntax: *SInfo"),
            info,
        ),
        Command::new(
            "SList",
            (0, 0),
            "Lists the system area's sprites.",
            Some("Syntax: *SList"),
            list,
        ),
        Command::new(
            "SLoad",
            (1, 1),
            "Loads a sprite file into the system area, replacing its sprites.",
            Some("Syntax: *SLoad <file>"),
            load,
        ),
        Command::new(
            "SMerge",
            (1, 1),
            "Merges a sprite file's sprites into the system area.",
            Some("Syntax: *SMerge <file>"),
            merge,
        ),
        Command::new(
            "SNew",
            (0, 0),
            "Deletes every sprite of the system area.",
            Some("Syntax: *SNew"),
            new,
        ),
        Command::new(
            "SRename",
            (2, 2),
            "Renames a sprite of the system area.",
            Some("Syntax: *SRename <name> <new name>"),
            rename,
        ),
        Command::new(
            "SSave",
            (1, 1),
            "Saves the system area as a sprite file.",
            Some("Syntax: *SSave <file>"),
            save,
        ),
    ],
    init: None,
    finalise: None,
    start: None,
    routines: &[],
    vectors: &[],
    resources: &[],
    code: None,
};

/// The command tail's parameters.
fn parameters(os: &mut Os<'_>, regs: &Regs) -> Result<Vec<Vec<u8>>, Fail> {
    let tail = os.read_text(regs.r[0], TAIL_MOST)?;
    let mut words = Vec::new();
    for (start, end) in command_parameters(&tail) {
        if words.try_reserve(1).is_err() {
            return Err(os.no_memory());
        }
        words.push(tail[start..end].to_vec());
    }
    Ok(words)
}

/// `OS_SpriteOp` `operation` on the system area, R2 and R3 the texts given
/// in guest memory where they are some.
fn sprite_op(
    os: &mut Os<'_>,
    operation: u32,
    r2: Option<&[u8]>,
    r3: Option<&[u8]>,
) -> Result<(), Fail> {
    let call = |os: &mut Os<'_>, r2: u32, r3: u32| {
        let mut regs = Regs::new([
            operation | Naming::System.value(),
            0,
            r2,
            r3,
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
        os.perform(X | SPRITE_OP, &mut regs)
    };
    match (r2, r3) {
        (Some(first), Some(second)) => os.with_text(first, 0, |os, first| {
            os.with_text(second, 0, |os, second| call(os, first, second))
        }),
        (Some(first), None) => os.with_text(first, 0, |os, first| call(os, first, 0)),
        _ => call(os, 0, 0),
    }
}

/// The one parameter `regs` names, for a call given that it has one.
fn first(os: &mut Os<'_>, regs: &Regs) -> Result<Vec<u8>, Fail> {
    Ok(parameters(os, regs)?.into_iter().next().unwrap_or_default())
}

fn choose(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let name = first(os, regs)?;
    sprite_op(os, reason::SELECT, Some(&name), None)
}

fn copy(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let words = parameters(os, regs)?;
    let (Some(old), Some(new)) = (words.first(), words.get(1)) else {
        return Ok(());
    };
    sprite_op(os, reason::COPY, Some(old), Some(new))
}

fn rename(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let words = parameters(os, regs)?;
    let (Some(old), Some(new)) = (words.first(), words.get(1)) else {
        return Ok(());
    };
    sprite_op(os, reason::RENAME, Some(old), Some(new))
}

fn delete(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    for name in parameters(os, regs)? {
        sprite_op(os, reason::DELETE, Some(&name), None)?;
    }
    Ok(())
}

fn flip_x(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let name = first(os, regs)?;
    sprite_op(os, reason::FLIP_X, Some(&name), None)
}

fn flip_y(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let name = first(os, regs)?;
    sprite_op(os, reason::FLIP_Y, Some(&name), None)
}

fn get(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let name = first(os, regs)?;
    sprite_op(os, reason::GET, Some(&name), None)
}

fn load(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let file = first(os, regs)?;
    sprite_op(os, reason::LOAD, Some(&file), None)
}

fn merge(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let file = first(os, regs)?;
    sprite_op(os, reason::MERGE, Some(&file), None)
}

fn save(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let file = first(os, regs)?;
    sprite_op(os, reason::SAVE, Some(&file), None)
}

fn new(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    sprite_op(os, reason::INITIALISE, None, None)
}

/// `*ScreenSave`: `OS_SpriteOp` 2 with the palette.
fn screen_save(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let file = first(os, regs)?;
    os.with_text(&file, 0, |os, at| {
        let mut call = Regs::new([reason::SCREEN_SAVE, 0, at, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        os.perform(X | SPRITE_OP, &mut call)
    })
}

fn screen_load(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let file = first(os, regs)?;
    os.with_text(&file, 0, |os, at| {
        let mut call = Regs::new([reason::SCREEN_LOAD, 0, at, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        os.perform(X | SPRITE_OP, &mut call)
    })
}

/// `*SList`: each sprite's name on a line of its own.
fn list(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let area = Area::system(os)?;
    let count = area.block(os)?.count;
    if count == 0 {
        return os.write_line(b"The system sprite area holds no sprites");
    }
    let mut walk = area.walk(os)?;
    while let Some(sprite) = walk.next(os)? {
        os.write_line(sprite.name.as_bytes())?;
    }
    Ok(())
}

/// `*SInfo`: the area's size, free space and count.
fn info(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let block = Area::system(os)?.block(os)?;
    let mut digits = [0; 20];
    let mut line = |os: &mut Os<'_>, value: u32, words: &[u8]| -> Result<(), Fail> {
        let number = tairix_util::fmt::format_u64(u64::from(value), &mut digits);
        os.write_text(b"  ")?;
        os.write_text(number.as_bytes())?;
        os.write_line(words)
    };
    os.write_line(b"The system sprite area:")?;
    line(os, block.size / 1024, b"K reserved")?;
    line(os, block.size.saturating_sub(block.free), b" bytes unused")?;
    line(os, block.count, b" sprites")
}
