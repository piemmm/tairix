//! The `OS_Byte` reasons the VDU drivers answer; every other passes to the
//! kernel. The variables RISC OS keeps in the `OS_Byte` table — the flash
//! counter and durations, the bell, the paged line count, the VDU queue and
//! the banks — stay there, read and written by the kernel's own rule.

use tairix_riscos_arm::psr;
use tairix_riscos_kernel::byte::var;
use tairix_riscos_kernel::os::{Fail, Os, Regs};

use crate::state::with;

/// `OS_Byte` 163,242,65's report that sprites and flood fills are always
/// there.
const SPRITES_AND_FLOODS: u32 = 0xC0;

/// The VDU drivers' `OS_Byte` reasons.
pub(crate) fn reason(os: &mut Os<'_>, regs: &mut Regs) -> Option<Result<(), Fail>> {
    let reason = regs.byte(0);
    Some(match reason {
        9 | 10 => {
            flash_duration(os, regs, reason == 9);
            Ok(())
        }
        19 => crate::frames::wait_for_frame(os),
        20 => reset_font(os, 32, 127),
        25 => match crate::font::group(regs.r[1]) {
            Some((first, last)) => reset_font(os, first, last),
            None => Ok(()),
        },
        106 => crate::pointer::select(os, regs),
        112 | 113 => banks(os, regs, reason == 112),
        114 => with(os, |vdu, _| {
            let old = u32::from(!vdu.shadow);
            vdu.shadow = regs.r[1] == 0;
            regs.r[1] = old;
            Ok(())
        }),
        117 => with(os, |vdu, _| {
            let t = &vdu.text;
            let mut status = 0;
            status |= u32::from(vdu.stream.printing);
            status |= u32::from(t.paged) << 2;
            status |= u32::from(t.windowed) << 3;
            status |= u32::from(vdu.screen.writing_second()) << 4;
            status |= u32::from(t.vdu5) << 5;
            status |= u32::from(t.editing.is_some()) << 6;
            status |= u32::from(vdu.stream.disabled) << 7;
            regs.r[1] = status;
            Ok(())
        }),
        134 | 165 => with(os, |vdu, _| {
            let (x, y) = match (reason, vdu.text.editing) {
                (134, Some(at)) => at,
                _ => (vdu.text.x, vdu.text.y),
            };
            regs.r[1] = x;
            regs.r[2] = y;
            Ok(())
        }),
        135 => with(os, |vdu, os| {
            let (x, y) = vdu.text.editing.unwrap_or((vdu.text.x, vdu.text.y));
            let (column, row) = vdu.text.cell(x, y);
            regs.r[1] = u32::from(vdu.read_char(os, column, row)?);
            regs.r[2] = match vdu.screen.named {
                crate::screen::Named::Number(n) => n & 0x7F,
                crate::screen::Named::Selector(address) => address,
            };
            Ok(())
        }),
        144 => with(os, |vdu, _| {
            let old = vdu.screen.tv;
            vdu.screen.tv = [regs.r[1] & 0xFF, regs.r[2] & 1];
            regs.r[1] = old[0];
            regs.r[2] = old[1];
            Ok(())
        }),
        160 => with(os, |vdu, _| {
            let (value, next) = crate::vars::byte_160(vdu, regs.r[1]);
            regs.r[1] = value;
            regs.r[2] = next;
            Ok(())
        }),
        163 if regs.r[1] == 242 => dots(os, regs),
        _ => return None,
    })
}

/// `OS_Byte` 9 and 10: the frames the first or second flash state lasts,
/// nought for ever; R1 the duration as it was.
fn flash_duration(os: &mut Os<'_>, regs: &mut Regs, first: bool) {
    let which = if first {
        var::FIRST_DURATION
    } else {
        var::SECOND_DURATION
    };
    os.swap_byte_var(which, regs);
}

/// Characters `first` to `last` reset: those below 128 to the system font,
/// those above through the current alphabet's definition.
fn reset_font(os: &mut Os<'_>, first: u8, last: u8) -> Result<(), Fail> {
    with(os, |vdu, _| {
        vdu.font.reset(first, last);
        Ok(())
    })?;
    if last >= 0x80 {
        let alphabet = os.read_alphabet()?;
        os.define_characters(alphabet, u32::from(first.max(0x80)), u32::from(last))?;
    }
    Ok(())
}

/// `OS_Byte` 112 and 113: the bank written to or shown, nought for the
/// mode's own; a bank past those the screen's memory holds is refused with
/// C set. R1 the bank as it was.
fn banks(os: &mut Os<'_>, regs: &mut Regs, written: bool) -> Result<(), Fail> {
    let asked = regs.r[1] & 0xFF;
    with(os, |vdu, os| {
        let screen = &mut vdu.screen;
        let bank = if asked == 0 { screen.own_bank } else { asked };
        let which = if written {
            var::VDU_BANK
        } else {
            var::DISPLAY_BANK
        };
        let old = u32::from(os.byte_var(which));
        if bank > screen.banks() {
            regs.r[2] = regs.r[1];
            regs.r[1] = old;
            regs.set_flag(psr::C, true);
            return Ok(());
        }
        screen.set_bank(written, bank);
        if !written {
            regs.r[2] = 0x200_0000;
        }
        os.set_byte_var(which, bank.to_le_bytes()[0]);
        regs.r[1] = old;
        regs.set_flag(psr::C, false);
        Ok(())
    })
}

/// `OS_Byte` 163,242: R2 0 the default dot pattern and its length, 1 to
/// 64 the length, 65 read the status, 66 the current sprite's size.
fn dots(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    with(os, |vdu, _| {
        match regs.r[2] {
            0 => {
                vdu.colours.dot = crate::colour::DEFAULT_DOT;
                vdu.colours.dot_length = crate::colour::DEFAULT_DOT_LENGTH;
                vdu.colours.dot_at = 0;
            }
            n @ 1..=64 => {
                vdu.colours.dot_length = n;
                vdu.colours.dot_at = 0;
            }
            65 => {
                regs.r[1] = SPRITES_AND_FLOODS | (vdu.colours.dot_length & 63);
                regs.r[2] = 0;
            }
            66 => {
                regs.r[1] = 0;
                regs.r[2] = 0;
            }
            _ => {}
        }
        Ok(())
    })
}
