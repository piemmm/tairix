//! Sprite files (`OS_SpriteOp` 10 to 12): an area saved from its count word
//! to its free space, typed `&FF9`; a file loaded over an area, or merged into
//! it, its sprites replacing any of the same name. A file loaded is held to
//! the form a saved area has before any of it is trusted: one that is not is
//! "Not a sprite file", and a load leaves its area empty.

use alloc::vec::Vec;

use tairix_image::{sprite_area as block, SpriteName, SPRITE_HEADER_LEN};
use tairix_riscos_kernel::filing::filetype;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::{file as reason, FILE};
use tairix_riscos_kernel::swi::X;

use super::area::{Area, Naming};
use super::header::Sprite;
use super::{system, BAD_FILE, NO_MEMORY, NO_SPRITES, UNRECOGNISED};

/// `OS_File` 5's object type for a file.
const IS_FILE: u32 = 1;

/// `OS_SpriteOp` 12: the area from its count word to its free space saved as
/// R2's file, typed as a sprite file; an area of no sprites is not saved.
pub(crate) fn save(os: &mut Os<'_>, naming: Naming, regs: &mut Regs) -> Result<(), Fail> {
    let area = Area::named(os, naming, regs.r[1])?;
    save_area(os, area, regs.r[2])
}

/// Saves `area` from its count word to its free space as file `name`.
pub(crate) fn save_area(os: &mut Os<'_>, area: Area, name: u32) -> Result<(), Fail> {
    let block = area.block(os)?;
    if block.count == 0 {
        return Err(os.error(NO_SPRITES));
    }
    let mut save = Regs::new([
        reason::SAVE_TYPED,
        name,
        filetype::SPRITE,
        0,
        area.address(block::COUNT),
        area.address(block.free),
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]);
    os.perform(X | FILE, &mut save)
}

/// `OS_SpriteOp` 10 and 11: R2's file loaded over the area, or where
/// `merge`d, after its sprites, any of theirs replacing one of the same name.
pub(crate) fn load(
    os: &mut Os<'_>,
    naming: Naming,
    regs: &mut Regs,
    merge: bool,
) -> Result<(), Fail> {
    let area = Area::named(os, naming, regs.r[1])?;
    let length = file_length(os, regs.r[2])?;
    let block = area.block(os)?;
    if merge {
        merge_file(os, area, regs.r[2], length)
    } else {
        load_file(os, area, block.first, regs.r[2], length)
    }
}

/// The length of file `name`; the error its kind gives where it is no file.
pub(crate) fn file_length(os: &mut Os<'_>, name: u32) -> Result<u32, Fail> {
    let mut info = Regs::new([reason::READ_INFO, name, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    os.perform(X | FILE, &mut info)?;
    if info.r[0] != IS_FILE {
        let mut refuse = Regs::new([
            reason::MAKE_ERROR,
            name,
            info.r[0],
            0,
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
        os.perform(X | FILE, &mut refuse)?;
        return Err(os.error(BAD_FILE));
    }
    Ok(info.r[4])
}

/// Loads file `name`, `length` bytes, at `address`.
fn load_at(os: &mut Os<'_>, name: u32, address: u32) -> Result<(), Fail> {
    let mut load = Regs::new([reason::LOAD, name, address, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    os.perform(X | FILE, &mut load)
}

/// Loads the file over the area, after its size word: refused where the area
/// cannot hold it, left empty where it is not a saved area.
pub(crate) fn load_file(
    os: &mut Os<'_>,
    area: Area,
    first: u32,
    name: u32,
    length: u32,
) -> Result<(), Fail> {
    let size = area.block(os)?.size;
    let fits = u64::from(length) + u64::from(block::COUNT) <= u64::from(size);
    if !fits
        || !os.writable(
            area.address(block::COUNT),
            u64::from(area.base) + u64::from(size),
        )
    {
        return Err(os.error(NO_MEMORY));
    }
    if area.system {
        system::select(os, None);
    }
    load_at(os, name, area.address(block::COUNT))?;
    let loaded = area.block(os)?;
    if !saved_area(
        os,
        area,
        loaded,
        u64::from(length) + u64::from(block::COUNT),
    )? {
        area.set(os, block::COUNT, 0)?;
        area.set(os, block::FIRST, first)?;
        area.set(os, block::FREE, first)?;
        return Err(os.error(BAD_FILE));
    }
    Ok(())
}

/// Whether `loaded`, an area's block, and the sprites it counts form a saved
/// area within `end` bytes of the area's start: the first sprite after the
/// block on a word, each sprite at least a control block long on a word, and
/// the last ending by the free space, which ends by the area's.
fn saved_area(
    os: &mut Os<'_>,
    area: Area,
    loaded: super::area::Block,
    end: u64,
) -> Result<bool, Fail> {
    let framed = loaded.first >= block::LEN
        && loaded.first.is_multiple_of(4)
        && loaded.first <= loaded.free
        && u64::from(loaded.free) <= end
        && loaded.free <= loaded.size;
    if !framed {
        return Ok(false);
    }
    let mut offset = u64::from(loaded.first);
    for _ in 0..loaded.count {
        if offset + u64::from(SPRITE_HEADER_LEN) > u64::from(loaded.free) {
            return Ok(false);
        }
        let next = os.read_word(area.address(u32::try_from(offset).unwrap_or(u32::MAX)))?;
        if next < SPRITE_HEADER_LEN || !next.is_multiple_of(4) {
            return Ok(false);
        }
        offset += u64::from(next);
        os.yield_point()?;
    }
    Ok(offset <= u64::from(loaded.free))
}

/// Merges the file into the area: loaded at its free space, the file's block
/// closed up so its sprites follow the area's, then any of the area's own
/// sprites a file sprite names removed.
fn merge_file(os: &mut Os<'_>, area: Area, name: u32, length: u32) -> Result<(), Fail> {
    let block = area.block(os)?;
    if block.free > block.size || block.first < block::LEN || block.free < block.first {
        return Err(os.error(UNRECOGNISED));
    }
    let end = u64::from(block.free) + u64::from(length);
    if end > u64::from(block.size)
        || !os.writable(area.address(block.free), u64::from(area.base) + end)
    {
        return Err(os.error(NO_MEMORY));
    }
    let staged = Area {
        base: area.address(block.free).wrapping_sub(block::COUNT),
        system: false,
    };
    load_at(os, name, area.address(block.free))?;
    let file = super::area::Block {
        size: length + block::COUNT,
        ..staged.block(os)?
    };
    if !saved_area(
        os,
        staged,
        file,
        u64::from(length) + u64::from(block::COUNT),
    )? {
        return Err(os.error(BAD_FILE));
    }
    let sprites = file.free - file.first;
    os.copy_bytes(
        staged.address(file.first),
        area.address(block.free),
        sprites,
    )?;
    area.set(os, block::FREE, block.free + sprites)?;
    area.set(os, block::COUNT, block.count.wrapping_add(file.count))?;
    let names = file_names(os, area, block.free, file.count)?;
    let mut walk_left = block.count;
    let mut offset = block.first;
    // The area's own sprites end where the file's begin, the end moving down
    // as each is removed: a count past them never reaches the file's.
    let mut own_end = block.free;
    while walk_left > 0 && offset < own_end {
        let sprite = Sprite::read(os, area.address(offset))?;
        let inside = sprite.next >= SPRITE_HEADER_LEN
            && sprite.next.is_multiple_of(4)
            && u64::from(offset) + u64::from(sprite.next) <= u64::from(own_end);
        if !inside {
            break;
        }
        walk_left -= 1;
        let lowered = sprite.name.lowered();
        if names
            .binary_search_by(|held| held.as_bytes().cmp(lowered.as_bytes()))
            .is_ok()
        {
            area.remove(os, &sprite)?;
            own_end -= sprite.next;
        } else {
            offset += sprite.next;
        }
        os.yield_point()?;
    }
    Ok(())
}

/// The names of the `count` sprites from `offset` of the area, in lower case,
/// sorted for searching.
fn file_names(
    os: &mut Os<'_>,
    area: Area,
    offset: u32,
    count: u32,
) -> Result<Vec<SpriteName>, Fail> {
    let mut names = Vec::new();
    if names.try_reserve_exact(count as usize).is_err() {
        return Err(os.error(NO_MEMORY));
    }
    let mut at = offset;
    for _ in 0..count {
        let sprite = Sprite::read(os, area.address(at))?;
        names.push(sprite.name.lowered());
        at = at.wrapping_add(sprite.next);
    }
    names.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    Ok(names)
}
