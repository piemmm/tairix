//! Screen dumps (`OS_SpriteOp` 2 and 3): the graphics window saved as a
//! sprite file of one sprite, `screendump`, with the screen's palette where
//! asked; and a sprite file's first sprite plotted at the graphics window's
//! bottom left, the screen first taking the sprite's mode where it differs
//! and its palette where it has one. Each works in a sprite area claimed
//! from the RMA for the call alone.

use tairix_image::{
    sprite_area as block, SpriteModeWord, SpriteName, SPRITE_HEADER_LEN, SPRITE_PALETTE_ENTRY_LEN,
};
use tairix_riscos_kernel::os::{len32, Entry, Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::{screen_mode, SCREEN_MODE};
use tairix_riscos_kernel::swi::X;
use tairix_vdu::mode::var;

use super::area::{Area, Naming};
use super::file;
use super::format;
use super::get::Grab;
use super::header::Sprite;
use super::output::shown_as_screen;
use super::paint;
use super::NO_MEMORY;
use crate::palette::{kind, reason as palette_reason, PALETTE_V};
use crate::select::{Selector, SELECTOR_BYTES};
use crate::state;

/// The name a screen dump's sprite takes.
const SCREEN_DUMP: &[u8] = b"screendump";

/// `OS_SpriteOp` 2: the graphics window saved as file R2, the screen's
/// palette with it where R3 is nonzero.
pub(crate) fn save(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let window = state::with(os, |vdu, _| Ok(vdu.graphics.window))?;
    let corners = ((window.left, window.bottom), (window.right, window.top));
    let grab = Grab::of(os, corners, regs.r[3] != 0)?;
    let size = grab
        .length()
        .and_then(|length| length.checked_add(block::LEN));
    let Some(size) = size else {
        return Err(os.error(NO_MEMORY));
    };
    let name = SpriteName::spelt_by(SCREEN_DUMP);
    in_scratch_area(os, size, |os, area| {
        grab.write(os, area, name)?;
        file::save_area(os, area, regs.r[2])
    })
}

/// `OS_SpriteOp` 3: file R2's first sprite plotted at the graphics window's
/// bottom left, in its own mode and palette.
pub(crate) fn load(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let length = file::file_length(os, regs.r[2])?;
    let Some(size) = length.checked_add(block::COUNT) else {
        return Err(os.error(NO_MEMORY));
    };
    in_scratch_area(os, size, |os, area| {
        file::load_file(os, area, block::LEN, regs.r[2], length)?;
        let Some(sprite) = area.nth(os, 1)? else {
            return Ok(());
        };
        adopt_mode(os, &sprite)?;
        adopt_palette(os, &sprite)?;
        let window = state::with(os, |vdu, _| Ok(vdu.graphics.window))?;
        paint::put(os, &sprite, (window.left, window.bottom), 0)
    })
}

/// Runs `work` on an empty sprite area of `size` bytes claimed from the RMA,
/// freed after.
fn in_scratch_area<T>(
    os: &mut Os<'_>,
    size: u32,
    work: impl FnOnce(&mut Os<'_>, Area) -> Result<T, Fail>,
) -> Result<T, Fail> {
    let base = os.rma_claim(size)?;
    let done = [
        (block::SIZE, size),
        (block::COUNT, 0),
        (block::FIRST, block::LEN),
        (block::FREE, block::LEN),
    ]
    .into_iter()
    .try_for_each(|(offset, value)| os.write_word(base.wrapping_add(offset), value))
    .and_then(|()| Area::named(os, Naming::Named, base))
    .and_then(|area| work(os, area));
    let freed = os.rma_free(base);
    let done = done?;
    freed.map(|()| done)
}

/// Selects the mode `sprite` was made in where it differs from the screen's:
/// its number, or for a newer sprite a mode of its pixels as wide and high as
/// it is. A mode the screen cannot take leaves the screen as it is.
fn adopt_mode(os: &mut Os<'_>, sprite: &Sprite) -> Result<(), Fail> {
    let current = state::with(os, |vdu, _| {
        Ok(format::of_screen(vdu.screen.named, vdu.format()))
    })?;
    if current == Some(sprite.mode) {
        return Ok(());
    }
    let mut regs = Regs::default();
    regs.r[0] = screen_mode::SELECT;
    let selected = if let SpriteModeWord::Number(number) = SpriteModeWord::of(sprite.mode) {
        regs.r[1] = u32::from(number);
        os.perform(X | SCREEN_MODE, &mut regs)
    } else {
        select_like(os, sprite, &mut regs)
    };
    match selected {
        Ok(()) | Err(Fail::Error(_)) => Ok(()),
        Err(unwind) => Err(unwind),
    }
}

/// Selects a mode of `sprite`'s pixels, eigen factors and colours as wide
/// and high as it is, through a selector below the supervisor stack: its
/// colours as it shows them, a full palette's among them.
fn select_like(os: &mut Os<'_>, sprite: &Sprite, regs: &mut Regs) -> Result<(), Fail> {
    let pixels = sprite.pixels(os)?;
    let shown = shown_as_screen(sprite, &pixels);
    let selector = Selector::new(
        sprite.width(&pixels),
        sprite.rows(),
        pixels.log2_bpp,
        u32::MAX,
    )
    .with([
        (var::X_EIG, shown.eig.0),
        (var::Y_EIG, shown.eig.1),
        (var::MODE_FLAGS, shown.flags),
        (var::N_COLOUR, shown.ncolour),
    ])
    .ok_or_else(|| os.error(format::UNKNOWN_MODE))?;
    let mut buffer = [0; SELECTOR_BYTES as usize];
    let bytes = selector.bytes(&mut buffer);
    os.below_supervisor_stack(len32(bytes.len()), |os, at| {
        os.write_bytes(at, bytes)?;
        regs.r[1] = at;
        os.perform(X | SCREEN_MODE, regs)
    })
}

/// Sets each palette entry `sprite` holds as the screen's, both flash
/// colours as it gives them.
fn adopt_palette(os: &mut Os<'_>, sprite: &Sprite) -> Result<(), Fail> {
    for entry in 0..sprite.named_entries(os)? {
        let at = sprite.address(SPRITE_HEADER_LEN + entry * SPRITE_PALETTE_ENTRY_LEN);
        let (first, second) = (os.read_word(at)?, os.read_word(at.wrapping_add(4))?);
        let mut set = Regs::new([
            entry,
            kind::NORMAL,
            first,
            second,
            palette_reason::SET,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
        os.call_vector(PALETTE_V, &mut set, Entry::Supervisor)?;
    }
    Ok(())
}
