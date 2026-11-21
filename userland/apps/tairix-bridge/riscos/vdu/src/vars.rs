//! The variables software reads: `OS_ReadVduVariables`'s VDU variables,
//! `OS_ReadModeVariable`'s mode variables for the current mode or one
//! named by number, selector or sprite mode word, and `OS_Byte` 160's
//! BBC-compatible bytes.

use tairix_riscos_arm::psr;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::output::workspace;

use crate::mode::Format;
use crate::select::{self, Outcome, Spec};
use crate::state::{with, Vdu};

/// The highest numbered mode the drivers know.
const MAX_MODE: u32 = 53;

impl Vdu {
    /// VDU variable `number`, if it is one: the mode's, then those from 128,
    /// then the window's size from 256.
    pub(crate) fn vdu_var(&self, os: &mut Os<'_>, number: u32) -> Option<u32> {
        if number < crate::mode::VARIABLES {
            return self.screen.format.current_var(number);
        }
        let g = &self.graphics;
        let t = &self.text;
        let format = &self.screen.format;
        let unsigned = |v: i32| v.cast_unsigned();
        Some(match number {
            128 => unsigned(g.window.left),
            129 => unsigned(g.window.bottom),
            130 => unsigned(g.window.right),
            131 => unsigned(g.window.top),
            132 => t.window.left,
            133 => t.window.bottom,
            134 => t.window.right,
            135 => t.window.top,
            136 => unsigned(g.origin.0),
            137 => unsigned(g.origin.1),
            138 => unsigned(g.cursor.0),
            139 => unsigned(g.cursor.1),
            140 => unsigned(g.older.0),
            141 => unsigned(g.older.1),
            142 => unsigned(g.old.0),
            143 => unsigned(g.old.1),
            144 => unsigned(g.cursor_pixels.0),
            145 => unsigned(g.cursor_pixels.1),
            146 => unsigned(g.new_point.0),
            147 => unsigned(g.new_point.1),
            148 => self.screen.screen_start(),
            149 => self.screen.display_start_address(),
            150 => self.screen.size,
            151 => self.colours.fore.action,
            152 => self.colours.back.action,
            153 => self.colours.fore.colour,
            154 => self.colours.back.colour,
            155 => self.colours.text_fore.colour,
            156 => self.colours.text_back.colour,
            157 => self.colours.fore.tint,
            158 => self.colours.back.tint,
            159 => self.colours.text_fore.tint,
            160 => self.colours.text_back.tint,
            161 => MAX_MODE,
            162 => g.char_size.0,
            163 => g.char_size.1,
            164 => g.char_space.0,
            165 => g.char_space.1,
            166 => os.vdu_routine(crate::routine::HLINE).unwrap_or(0),
            167 | 169 => format.cell().size_x,
            168 => format.cell().size_y,
            170 => format.cell().space_y,
            171 => os.vector_base() + workspace::FOREGROUND,
            172 => 0,
            256 => t.window.right - t.window.left,
            257 => t.window.bottom - t.window.top,
            _ => return None,
        })
    }
}

/// `OS_ReadVduVariables`: R0 a list of variable numbers ending -1, R1 where
/// their values go, nought for each that is none.
pub(crate) fn read_vdu_variables(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (mut list, mut out) = (regs.r[0], regs.r[1]);
    loop {
        let number = os.read_word(list)?;
        if number == u32::MAX {
            return Ok(());
        }
        let value = with(os, |vdu, os| Ok(vdu.vdu_var(os, number).unwrap_or(0)))?;
        os.write_word(out, value)?;
        list = list.wrapping_add(4);
        out = out.wrapping_add(4);
        os.yield_point()?;
    }
}

/// `OS_ReadModeVariable`: R0 the current mode as -1, a number, a selector
/// or a sprite mode word; R1 the variable; R2 its value with C clear, or C
/// set where either is none.
pub(crate) fn read_mode_variable(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (mode, number) = (regs.r[0], regs.r[1]);
    let value = if mode == u32::MAX {
        with(os, |vdu, _| Ok(vdu.screen.format.current_var(number)))?
    } else if mode & 1 != 0 && mode >= 0x100 && mode >> 27 != 0 {
        Format::sprite_word(mode).and_then(|sprite| sprite.var(number))
    } else {
        match select::read_spec(os, mode)? {
            Some(spec) => named_var(os, &spec, number)?,
            None => None,
        }
    };
    regs.r[2] = value.unwrap_or(0);
    regs.set_flag(psr::C, value.is_none());
    Ok(())
}

/// Variable `number` of the mode `spec` names, as a mode named reads it.
fn named_var(os: &mut Os<'_>, spec: &Spec, number: u32) -> Result<Option<u32>, Fail> {
    if let Spec::Number(n) = spec {
        if let Some(m) = tairix_vdu::mode::numbered(n & 0x7F) {
            return Ok(Format::numbered(m).var(number));
        }
    }
    let monitor = with(os, |vdu, _| Ok(vdu.monitor))?;
    Ok(match select::resolve(os, spec, monitor)? {
        Outcome::Valid(resolved) => resolved.format.var(number),
        Outcome::Substitute(_) | Outcome::Unknown => None,
    })
}

/// `OS_Byte` 160: byte `index` of the BBC's VDU variables — the graphics
/// window in pixels, the text window, the origin in OS units — R1 its value
/// and R2 the next's.
pub(crate) fn byte_160(vdu: &Vdu, index: u32) -> (u32, u32) {
    let g = &vdu.graphics;
    let t = &vdu.text;
    let half = |v: i32, high: bool| u32::from(v.to_le_bytes()[usize::from(high)]);
    let byte = |i: u32| -> u32 {
        match i {
            0 | 1 => half(g.window.left, i == 1),
            2 | 3 => half(g.window.bottom, i == 3),
            4 | 5 => half(g.window.right, i == 5),
            6 | 7 => half(g.window.top, i == 7),
            8 => t.window.left,
            9 => t.window.bottom,
            10 => t.window.right,
            11 => t.window.top,
            12 | 13 => half(g.origin.0, i == 13),
            14 | 15 => half(g.origin.1, i == 15),
            _ => 0,
        }
    };
    (byte(index), index.checked_add(1).map_or(0, byte))
}

/// The colour number and tint of the pixel value `pixel` in `format`.
pub(crate) fn number_of(format: &Format, pixel: u32) -> (u32, u32) {
    if format.log2_bpp() == 3 && format.grouped() {
        crate::colour::number_of_internal(pixel.to_le_bytes()[0])
    } else {
        (pixel, 0)
    }
}
