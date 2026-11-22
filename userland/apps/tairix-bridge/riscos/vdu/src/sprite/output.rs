//! Output switched to a sprite (`OS_SpriteOp` 60 to 62): the drivers draw on
//! a sprite's image or mask as though it were the screen, until output is
//! switched elsewhere. Its mode variables are the sprite's but for its screen
//! size, which stays the screen's; an old-format sprite's left-hand wastage
//! is removed first, as `OS_SpriteOp` 54 removes it, and a sprite with no
//! pixels is refused.
//!
//! A switch keeps the state of the destination left in the save area it was
//! switched to with, and takes the new one's from the save area given: none,
//! the drivers' own, or a program's ([`crate::saved`]). Whether a program's
//! area holds a state is decided before the old one is kept, so one never
//! written starts its destination afresh even when the destination left was
//! kept in it, as RISC OS 5 has it; the drivers' own always holds one.

use tairix_image::{sprite_dpi_of_eig, sprite_eig_of_dpi, sprite_type, SpriteModeWord};
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_vdu::mode::{var, Flags};

use super::area::{Area, Naming};
use super::create::Target;
use super::edit;
use super::format::{self, Pixels};
use super::header::Sprite;
use super::{reason, FULL_PALETTE, UNRECOGNISED};
use crate::canvas::Canvas;
use crate::mode::{Drawn, Format};
use crate::output::{Context, Output, KERNEL_SAVE_AREA, NO_SAVE_AREA};
use crate::saved::{Saved, SAVE_AREA_BYTES};
use crate::state;

/// `OS_SpriteOp` 62: R3 the bytes a save area takes, for the screen where R2
/// is nought or for R2's sprite.
pub(crate) fn save_area_size(os: &mut Os<'_>, naming: Naming, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[2] != 0 {
        let area = Area::named(os, naming, regs.r[1])?;
        area.sprite(os, naming, regs.r[2])?;
    }
    regs.r[3] = SAVE_AREA_BYTES;
    Ok(())
}

/// `OS_SpriteOp` 60 and 61: output switched to R2's sprite's image or mask,
/// or the screen where R2 is nought, R3 the save area; R0 to R3 what switches
/// output back.
pub(crate) fn switch(os: &mut Os<'_>, naming: Naming, regs: &mut Regs) -> Result<(), Fail> {
    let asked = [regs.r[0], regs.r[1], regs.r[2], regs.r[3]];
    let operation = asked[0] & 0xFF;
    let by_pointer = Naming::Pointer.value() | operation;
    let (output, context) = if asked[2] == 0 {
        (Output::Screen, Context([by_pointer, 0, 0, asked[3]]))
    } else {
        let area = Area::named(os, naming, asked[1])?;
        let sprite = area.sprite(os, naming, asked[2])?;
        area.holds(os, &sprite)?;
        if sprite.first_bit != 0 {
            let target = Target {
                area,
                naming,
                sprite,
            };
            edit::remove_wastage(os, &target)?;
        }
        let sprite = Sprite::read(os, sprite.at)?;
        let output = redirected(os, &sprite, operation == reason::SWITCH_TO_MASK)?;
        (
            output,
            Context([by_pointer, area.base, sprite.at, asked[3]]),
        )
    };
    let initialised = match asked[3] {
        NO_SAVE_AREA => false,
        KERNEL_SAVE_AREA => true,
        address => os.read_word(address)? != 0,
    };
    let (previous, left) = state::with(os, |vdu, os| Ok((vdu.context, Saved::take(vdu, os)?)))?;
    keep(os, previous.save_area(), left)?;
    let arriving = if initialised {
        recall(os, asked[3])?
    } else {
        None
    };
    state::with(os, |vdu, os| {
        vdu.output = output;
        vdu.context = context;
        let state = arriving.unwrap_or_else(|| Saved::fresh(vdu.format(), vdu.text.flags));
        state.restore(vdu, os)
    })?;
    regs.r[..4].copy_from_slice(&previous.0);
    crate::output::announce(os, asked)
}

/// Keeps `saved` where save area `save_area` says.
fn keep(os: &mut Os<'_>, save_area: u32, saved: Saved) -> Result<(), Fail> {
    match save_area {
        NO_SAVE_AREA => Ok(()),
        KERNEL_SAVE_AREA => state::with(os, |vdu, _| {
            vdu.kernel_save = Some(saved);
            Ok(())
        }),
        address => saved.write(os, address),
    }
}

/// The state save area `save_area` holds, if any.
fn recall(os: &mut Os<'_>, save_area: u32) -> Result<Option<Saved>, Fail> {
    match save_area {
        NO_SAVE_AREA => Ok(None),
        KERNEL_SAVE_AREA => state::with(os, |vdu, _| Ok(vdu.kernel_save.clone())),
        address => Saved::read(os, address),
    }
}

/// `sprite`'s image, or its mask where `mask`, as output switched to it draws
/// on it; a sprite without a mask is drawn on in its image either way.
fn redirected(os: &mut Os<'_>, sprite: &Sprite, mask: bool) -> Result<Output, Fail> {
    let pixels = sprite.pixels(os)?;
    sprite.whole(os, &pixels)?;
    let (width, height) = (sprite.width(&pixels), sprite.rows());
    if width == 0 || height == 0 {
        return Err(os.error(UNRECOGNISED));
    }
    let word = SpriteModeWord::of(sprite.mode);
    let mask = mask && sprite.masked();
    let (drawn, start, word) = if mask && !pixels.old_format {
        let wide = pixels.wide_mask;
        let (log2_bpp, ncolour, flags, depth) = if wide {
            let flags = Flags::GREYSCALE.union(Flags::FULL_PALETTE).bits();
            (3, 0xFF, flags, sprite_type::PALETTED_8)
        } else {
            (0, 1, 0, sprite_type::PALETTED_1)
        };
        let eig = eig_of(word);
        let dpi = (sprite_dpi_of_eig(eig.0), sprite_dpi_of_eig(eig.1));
        let drawn = Drawn {
            flags: flags | Flags::HARD_SCROLL_DISABLED.bits(),
            ncolour,
            eig: (u32::from(eig.0), u32::from(eig.1)),
            y_shift: 0,
            log2_bpp,
            log2_bpc: log2_bpp,
            line_length: line_length(os, sprite.mask_stride(&pixels))?,
            width,
            height,
        };
        let word = SpriteModeWord::dpi_word(depth, dpi, false);
        (drawn, sprite.mask, word)
    } else {
        let drawn = Drawn {
            line_length: line_length(os, sprite.stride())?,
            width,
            height,
            ..image_drawn(sprite, &pixels, word)
        };
        (
            drawn,
            if mask { sprite.mask } else { sprite.image },
            sprite.mode,
        )
    };
    Ok(Output::Sprite {
        format: Format::sprite(&drawn),
        canvas: Canvas {
            start: sprite.address(start),
            line: drawn.line_length,
            width,
            height,
            log2_bpc: drawn.log2_bpc,
        },
        word,
    })
}

/// A row's bytes as a line length; one no sprite could hold is refused.
fn line_length(os: &mut Os<'_>, stride: u64) -> Result<u32, Fail> {
    u32::try_from(stride).map_err(|_| os.error(UNRECOGNISED))
}

/// How a sprite's image is drawn as a screen, its geometry apart: a numbered
/// mode's own variables, else its type's, eight bits of colour the full
/// palette a palette of 256 entries gives.
fn image_drawn(sprite: &Sprite, pixels: &Pixels, word: SpriteModeWord) -> Drawn {
    let full_palette = pixels.log2_bpp == 3 && sprite.palette_entries() == FULL_PALETTE;
    let palette_flags = if full_palette {
        Flags::FULL_PALETTE.bits()
    } else {
        0
    };
    let base = Drawn {
        flags: 0,
        ncolour: 0,
        eig: (0, 0),
        y_shift: 0,
        log2_bpp: pixels.log2_bpp,
        log2_bpc: pixels.log2_field,
        line_length: 0,
        width: 0,
        height: 0,
    };
    let numbered = match word {
        SpriteModeWord::Number(number) => tairix_vdu::mode::numbered(u32::from(number)),
        _ => None,
    };
    let drawn = if let Some(mode) = numbered {
        let mode = Format::numbered(mode);
        Drawn {
            flags: mode.flags(),
            ncolour: mode.ncolour(),
            eig: (mode.x_eig(), mode.y_eig()),
            y_shift: mode.var(var::Y_SHIFT).unwrap_or(0),
            ..base
        }
    } else {
        let eig = eig_of(word);
        Drawn {
            flags: format::variable(sprite.mode, var::MODE_FLAGS).unwrap_or(0),
            ncolour: match pixels.log2_bpp {
                0 => 1,
                1 => 3,
                2 => 15,
                3 => 63,
                _ => format::variable(sprite.mode, var::N_COLOUR).unwrap_or(u32::MAX),
            },
            eig: (u32::from(eig.0), u32::from(eig.1)),
            ..base
        }
    };
    Drawn {
        flags: drawn.flags | palette_flags | Flags::HARD_SCROLL_DISABLED.bits(),
        ncolour: if full_palette { 0xFF } else { drawn.ncolour },
        ..drawn
    }
}

/// The eigen factors a new-format word states.
fn eig_of(word: SpriteModeWord) -> (u8, u8) {
    match word {
        SpriteModeWord::Dpi { dpi, .. } => (
            sprite_eig_of_dpi(u32::from(dpi.0)),
            sprite_eig_of_dpi(u32::from(dpi.1)),
        ),
        SpriteModeWord::Eig { eig, .. } => eig,
        SpriteModeWord::Number(_) | SpriteModeWord::Selector => (1, 1),
    }
}
