//! The system sprite area, dynamic area 3: made at reset as large as
//! `*Configure SpriteSize` says, its handler keeping the area's size word
//! and refusing to shrink it past its sprites; and the sprite the system
//! area has selected for `PLOT` &E8–&EF, `OS_Byte` 163 and `VDU 23,27`.

use tairix_image::{sprite_area as block, SpriteName};
use tairix_riscos_arm::mmu::PAGE_SIZE;
use tairix_riscos_kernel::cmos;
use tairix_riscos_kernel::memory::handler_reason as reason;
use tairix_riscos_kernel::memory::{Bits, Create};
use tairix_riscos_kernel::os::{Fail, Os, Regs};

use super::area::Area;
use crate::state;

/// The area's number.
pub(crate) const AREA: u32 = 3;
/// The most it may reach, as RISC OS 5 has it, and in a layout that admits
/// 26-bit code the share of the 26-bit window's areas it may take.
const MOST: u32 = 0x100_0000;
const MOST_26BIT: u32 = 0x40_0000;
/// Its title.
const TITLE: &[u8] = b"System sprites";

/// What the drivers keep of sprites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Sprites {
    /// The system area's first byte, while dynamic area 3 holds anything.
    pub(crate) system: Option<u32>,
    /// The bytes dynamic area 3 holds.
    pub(crate) system_size: u32,
    /// The system area's selected sprite, by name: a sprite moved by another's
    /// deletion stays selected.
    pub(crate) selected: Option<SpriteName>,
}

impl Sprites {
    /// Before dynamic area 3 is made.
    pub(crate) const fn new() -> Self {
        Self {
            system: None,
            system_size: 0,
            selected: None,
        }
    }
}

/// Makes dynamic area 3 at reset, as large as CMOS says, its handler the
/// drivers' routine at `handler`; the drivers' state must be held already, as
/// the handler records each change in it.
pub(crate) fn make_area(os: &mut Os<'_>, handler: u32) -> Result<(), Fail> {
    let pages = os.cmos_pages(cmos::SPRITE_SIZE)?;
    let caller = os.layout().kernel_bits();
    let most = if caller == Bits::Bit26 {
        MOST_26BIT
    } else {
        MOST
    };
    os.create_kernel_area(&Create {
        number: Some(AREA),
        size: (u32::from(pages) * PAGE_SIZE).min(most),
        base: None,
        flags: 0,
        max: Some(most),
        handler: Some(handler),
        workspace: None,
        title: Some(TITLE),
        pool: 0,
        caller,
    })?;
    Ok(())
}

/// The area's handler: a shrink is held to the pages the area's sprites
/// reach, an area of none being free to go; after any change the area's size
/// word says its size, and an area grown from nothing is made an empty sprite
/// area.
pub(crate) fn area_handler(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match regs.r[0] {
        reason::PRE_SHRINK => {
            let size = regs.r[4];
            let used = match state::read(os, |vdu, _| vdu.sprites.system).flatten() {
                Some(base) => match os.read_word(base.wrapping_add(block::COUNT)) {
                    Ok(0) => 0,
                    _ => os.read_word(base.wrapping_add(block::FREE)).unwrap_or(size),
                },
                None => 0,
            };
            let spare = size.saturating_sub(used.min(size)) & !(PAGE_SIZE - 1);
            regs.r[3] = regs.r[3].min(spare);
        }
        reason::POST_GROW | reason::POST_SHRINK => {
            let (base, size) = os.area_extent(AREA)?;
            let before = state::read(os, |vdu, _| {
                let before = vdu.sprites.system_size;
                vdu.sprites.system = (size != 0).then_some(base);
                vdu.sprites.system_size = size;
                before
            });
            if size != 0 {
                let system = Area { base, system: true };
                system.set(os, block::SIZE, size)?;
                if before == Some(0) {
                    system.set(os, block::COUNT, 0)?;
                    system.set(os, block::FIRST, block::LEN)?;
                    system.set(os, block::FREE, block::LEN)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// Selects `name` in the system area, as `OS_SpriteOp` 24 and `VDU 23,27,0`
/// do; nothing where the drivers are not running.
pub(crate) fn select(os: &mut Os<'_>, name: Option<SpriteName>) {
    let _ = state::read(os, |vdu, _| vdu.sprites.selected = name);
}

/// The selected sprite's pixels across and down, as `OS_Byte` 163,242,66
/// reads them: nought for none, or one no longer in the system area.
pub(crate) fn selected_size(os: &mut Os<'_>) -> Result<(u32, u32), Fail> {
    let Some(name) = state::read(os, |vdu, _| vdu.sprites.selected).flatten() else {
        return Ok((0, 0));
    };
    let Ok(system) = Area::system(os) else {
        return Ok((0, 0));
    };
    let Some(sprite) = system.find(os, &name)? else {
        return Ok((0, 0));
    };
    let pixels = sprite.pixels(os)?;
    Ok((sprite.width(&pixels), sprite.rows()))
}

/// The bytes dynamic area 3 holds, as `OS_Byte` 163,242,65 reads them.
pub(crate) fn size(os: &mut Os<'_>) -> u32 {
    state::read(os, |vdu, _| vdu.sprites.system_size).unwrap_or(0)
}
