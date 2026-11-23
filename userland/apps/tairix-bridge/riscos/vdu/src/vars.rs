//! The variables software reads: `OS_ReadVduVariables`'s VDU variables,
//! `OS_ReadModeVariable`'s mode variables for the current mode or one
//! named by number, selector or sprite mode word, and `OS_Byte` 160's
//! BBC-compatible bytes.

use tairix_riscos_arm::psr;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::output::workspace;
use tairix_vdu::mode::vdu_var;

use crate::mode::{var, Format};
use crate::output::Output;
use crate::select::{self, Outcome, Spec};
use crate::state::{with, Vdu};

/// The highest numbered mode the drivers know.
const MAX_MODE: u32 = 53;

impl Vdu {
    /// Mode variable `number` of the destination drawn: a sprite's as it is
    /// made, its screen size the screen's.
    pub(crate) fn mode_var(&self, number: u32) -> Option<u32> {
        match self.output {
            Output::Screen => self.screen.format.current_var(number),
            Output::Sprite { format, .. } => match number {
                var::SCREEN_SIZE => self.screen.format.current_var(number),
                _ => format.var(number),
            },
        }
    }

    /// VDU variable `number`, if it is one: the mode's, then those from 128,
    /// then the window's size from 256.
    pub(crate) fn vdu_var(&self, os: &mut Os<'_>, number: u32) -> Option<u32> {
        if number < crate::mode::VARIABLES {
            return self.mode_var(number);
        }
        let g = &self.graphics;
        let t = &self.text;
        let format = self.format();
        let unsigned = |v: i32| v.cast_unsigned();
        // A window no VDU 28 set reads as none along an axis a sprite smaller
        // than a cell has no cell on.
        let none_or = |cells: u32, edge: u32| {
            if cells == 0 && !t.windowed {
                u32::MAX
            } else {
                edge
            }
        };
        let right = none_or(format.columns(), t.window.right);
        let bottom = none_or(format.rows(), t.window.bottom);
        Some(match number {
            128 => unsigned(g.window.left),
            129 => unsigned(g.window.bottom),
            130 => unsigned(g.window.right),
            131 => unsigned(g.window.top),
            132 => t.window.left,
            133 => bottom,
            134 => right,
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
            148 => self.canvas().start,
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
            vdu_var::WINDOW_WIDTH => right.wrapping_sub(t.window.left),
            vdu_var::WINDOW_HEIGHT => bottom.wrapping_sub(t.window.top),
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
    let [value] = mode_variables(os, regs.r[0], [regs.r[1]])?;
    regs.r[2] = value.unwrap_or(0);
    regs.set_flag(psr::C, value.is_none());
    Ok(())
}

/// Variables `numbers` of `mode`, named as `OS_ReadModeVariable` takes it,
/// the mode read once: none where either is none.
pub(crate) fn mode_variables<const N: usize>(
    os: &mut Os<'_>,
    mode: u32,
    numbers: [u32; N],
) -> Result<[Option<u32>; N], Fail> {
    if mode == u32::MAX {
        return with(os, |vdu, _| Ok(numbers.map(|number| vdu.mode_var(number))));
    }
    if mode & 1 != 0 && mode >= 0x100 && mode >> 27 != 0 {
        return Ok(numbers.map(|number| crate::sprite::mode_variable(mode, number)));
    }
    let Some(spec) = select::read_spec(os, mode)? else {
        return Ok([None; N]);
    };
    if let Spec::Number(n) = spec {
        if let Some(m) = tairix_vdu::mode::numbered(n & 0x7F) {
            let format = Format::numbered(m);
            return Ok(numbers.map(|number| format.var(number)));
        }
    }
    let monitor = with(os, |vdu, _| Ok(vdu.monitor))?;
    Ok(match select::resolve(os, &spec, monitor)? {
        Outcome::Valid(resolved) => numbers.map(|number| resolved.format.var(number)),
        Outcome::Substitute(_) | Outcome::Unknown => [None; N],
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
    if format.grouped() {
        crate::colour::number_of_internal(pixel.to_le_bytes()[0])
    } else {
        (pixel, 0)
    }
}
