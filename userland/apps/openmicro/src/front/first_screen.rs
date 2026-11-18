//! The screen a machine shows once it has started, before anything is put
//! in: its model's boot screen as the clean-room ROMs draw it, which the
//! library's card of a machine never yet run shows. `Run` carries each one
//! packed ([`crate::firmware`]), drawn by the tree on the host.

use alloc::vec::Vec;

use tairix_crt::screen::DOT;

use super::picture::{expand, palette_of};
use crate::firmware::{C64_SCREEN_IMAGE, ELECTRON_SCREEN_IMAGE, MODEL_B_SCREEN_IMAGE};
use crate::machine::canvas::{CANVAS_HEIGHT, CANVAS_WIDTH};
use crate::machine::Model;

/// `model`'s boot screen as `Run` carries it: the canvas, a slot a dot,
/// packed.
const fn packed(model: Model) -> &'static [u8] {
    match model {
        Model::ModelB => MODEL_B_SCREEN_IMAGE,
        Model::Electron => ELECTRON_SCREEN_IMAGE,
        Model::C64 => C64_SCREEN_IMAGE,
    }
}

/// `model`'s boot screen drawn at `size`, red, green, blue and alpha, as a
/// card shows a machine's last screen; `None` for a size too small for the
/// monitor's shape, or one the heap will not hold.
#[must_use]
pub fn picture(model: Model, size: (u32, u32)) -> Option<Vec<u8>> {
    let packed = packed(model);
    let slots_len = CANVAS_WIDTH * CANVAS_HEIGHT;
    if tairix_compress::decompressed_len(packed).ok()? != slots_len {
        return None;
    }
    let mut slots = tairix_util::fallible::filled(slots_len, 0u8)?;
    tairix_compress::decompress(packed, &mut slots).ok()?;
    let mut dots = tairix_util::fallible::filled(slots_len.checked_mul(DOT)?, 0u8)?;
    expand(&palette_of(model), &slots, &mut dots);
    let (width, height) = (
        u32::try_from(CANVAS_WIDTH).ok()?,
        u32::try_from(CANVAS_HEIGHT).ok()?,
    );
    tairix_crt::screen::thumbnail(width, height, &dots, size)
}

#[cfg(test)]
#[path = "first_screen_tests.rs"]
mod tests;
