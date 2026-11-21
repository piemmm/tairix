//! The OS SWIs the VDU drivers perform.

use tairix_riscos_arm::psr;
use tairix_riscos_kernel::os::{Entry, Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::{self as number, set_colour};

use crate::palette::PALETTE_V;
use crate::state::with;

/// Performs OS SWI `number`, one of those the kernel routes here.
pub(crate) fn perform(os: &mut Os<'_>, number: u32, regs: &mut Regs) -> Result<(), Fail> {
    match number & !tairix_riscos_kernel::swi::X {
        number::READ_PALETTE => {
            let mut palette = Regs::new([regs.r[0], regs.r[1], 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
            os.call_vector(PALETTE_V, &mut palette, Entry::Supervisor)?;
            regs.r[2] = palette.r[2];
            regs.r[3] = palette.r[3];
            Ok(())
        }
        number::READ_VDU_VARIABLES => crate::vars::read_vdu_variables(os, regs),
        number::READ_POINT => read_point(os, regs),
        number::READ_MODE_VARIABLE => crate::vars::read_mode_variable(os, regs),
        number::REMOVE_CURSORS => with(os, |vdu, _| {
            vdu.cursors_removed = vdu.cursors_removed.saturating_add(1);
            Ok(())
        }),
        number::RESTORE_CURSORS => with(os, |vdu, _| {
            vdu.cursors_removed = vdu.cursors_removed.saturating_sub(1);
            Ok(())
        }),
        number::CHECK_MODE_VALID => crate::select::check_mode_valid(os, regs),
        number::CLAIM_SCREEN_MEMORY => claim_screen_memory(os, regs),
        number::SET_ECF_ORIGIN => with(os, |vdu, os| {
            let (x, y) = (regs.r[0].cast_signed(), regs.r[1].cast_signed());
            vdu.colours.ecf_origin = vdu.pixels_of(x, y);
            let format = vdu.screen.format;
            vdu.colours.publish(os, &format)
        }),
        number::CHANGED_BOX => crate::changed::swi(os, regs),
        number::SET_COLOUR => set_colour(os, regs),
        number::POINTER => {
            if regs.r[0] == 0 {
                regs.r[0] = 0;
            }
            Ok(())
        }
        number::SCREEN_MODE => crate::select::screen_mode(os, regs),
        _ => Err(os.error(tairix_riscos_kernel::error::BAD_PARAMETERS)),
    }
}

/// `OS_ReadPoint`: the colour and tint at R0, R1, in OS units from the
/// origin, in R2 and R3, R4 nought; off the screen R2 and R4 -1.
fn read_point(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (x, y) = (regs.r[0].cast_signed(), regs.r[1].cast_signed());
    let colour = with(os, |vdu, os| vdu.colour_at(os, x, y))?;
    (regs.r[2], regs.r[3], regs.r[4]) =
        colour.map_or((u32::MAX, 0, u32::MAX), |(c, tint)| (c, tint, 0));
    Ok(())
}

impl crate::state::Vdu {
    /// The colour number and tint of the pixel at `(x, y)` in OS units from
    /// the origin, where it is on the screen and the mode has pixels.
    pub(crate) fn colour_at(
        &self,
        os: &mut Os<'_>,
        x: i32,
        y: i32,
    ) -> Result<Option<(u32, u32)>, Fail> {
        let (px, py) = self.pixels_of(x, y);
        let format = self.screen.format;
        let inside = px >= 0
            && py >= 0
            && px < format.width().cast_signed()
            && py < format.height().cast_signed();
        if !inside || !format.graphic() {
            return Ok(None);
        }
        let canvas = self.screen.canvas();
        let pixel = canvas.pixel(os, canvas.row_of(py), px.cast_unsigned())?;
        let pixel = pixel & format.pixel_mask();
        Ok(Some(crate::vars::number_of(&format, pixel)))
    }
}

/// `OS_ClaimScreenMemory`: R0 1 claims R1 bytes of the screen's memory the
/// current screen does not use, R2 their address and C clear; R0 0 gives
/// them back. The drivers lend at most one block at once, as RISC OS 5
/// does.
fn claim_screen_memory(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    with(os, |vdu, _| {
        let screen = &vdu.screen;
        let spare = screen.size.saturating_sub(screen.needed());
        let lent = vdu.screen.lent;
        if regs.r[0] == 0 {
            vdu.screen.lent = false;
            regs.set_flag(psr::C, false);
            return Ok(());
        }
        if lent || regs.r[1] > spare {
            regs.r[1] = spare;
            regs.set_flag(psr::C, true);
            return Ok(());
        }
        regs.r[2] = screen.base + screen.needed();
        vdu.screen.lent = true;
        regs.set_flag(psr::C, false);
        Ok(())
    })
}

/// `OS_SetColour`: R0 bits 0 to 3 the action, bit 4 the background, bit 5
/// R1 a pattern block of a word a row rather than a colour number, bit 6 a
/// text colour rather than a graphics one, bit 7 read the colour rather
/// than set it.
fn set_colour(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let flags = regs.r[0];
    let background = flags & set_colour::BACKGROUND != 0;
    let text = flags & set_colour::TEXT != 0;
    if flags & set_colour::READ != 0 {
        let words = with(os, |vdu, _| {
            let format = vdu.screen.format;
            if text {
                let (fore, back) = vdu.colours.text_words(&format);
                return Ok(Err(if background { back } else { fore }));
            }
            let gcol = if background {
                vdu.colours.back
            } else {
                vdu.colours.fore
            };
            Ok(Ok(gcol.pattern.unwrap_or_else(|| {
                [crate::colour::replicate(
                    &format,
                    crate::colour::Colours::gcol_pixel(&format, &gcol),
                ); 8]
            })))
        })?;
        match words {
            Ok(pattern) => {
                for (i, word) in (0u32..).zip(pattern) {
                    os.write_word(regs.r[1].wrapping_add(4 * i), word)?;
                }
            }
            Err(colour) => regs.r[1] = colour,
        }
        return Ok(());
    }
    let pattern = if flags & set_colour::PATTERN != 0 {
        let mut words = [0; 8];
        for (i, word) in (0u32..).zip(words.iter_mut()) {
            *word = os.read_word(regs.r[1].wrapping_add(4 * i))?;
        }
        Some(words)
    } else {
        None
    };
    with(os, |vdu, os| {
        let format = vdu.screen.format;
        if text {
            let target = if background {
                &mut vdu.colours.text_back
            } else {
                &mut vdu.colours.text_fore
            };
            target.colour = regs.r[1];
            target.literal = true;
            return Ok(());
        }
        let target = if background {
            &mut vdu.colours.back
        } else {
            &mut vdu.colours.fore
        };
        target.action = flags & set_colour::ACTION;
        target.literal = true;
        target.pattern = pattern;
        if pattern.is_none() {
            target.colour = regs.r[1];
        }
        vdu.colours.publish(os, &format)
    })
}
