//! The VDU stream (`plans/RISCOS-EMULATOR.md` §8): each byte a character or
//! a control code, a code's parameters gathered in the queue whose count of
//! bytes still to come is `OS_Byte` 218's variable, as RISC OS keeps it, so
//! flushing the keyboard buffer or writing the variable abandons a sequence.
//!
//! Work that calls out — the printer, the bell, a mode change, the palette
//! through `PaletteV`, the unknown codes' vectors, the keys a scroll waits
//! on — is an [`Effect`] the stream makes between two uses of the drivers'
//! state, never inside one.

use tairix_riscos_kernel::byte::var;
use tairix_riscos_kernel::os::{Entry, Fail, Os, Regs};
use tairix_riscos_kernel::vectors::Vector;
use tairix_vdu::code::{Control, Operation, FIRST_DEFINED};

use crate::plot::Ink;
use crate::state::{with, Vdu};
use crate::text::Needs;

/// `UKVDU23V` and `UKPlotV`.
const UK_VDU23: Vector = Vector(0x17);
const UK_PLOT: Vector = Vector(0x19);

/// The stream's own state: the code being gathered and its parameters, and
/// the switches its codes set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stream {
    pub(crate) code: u8,
    pub(crate) gathered: [u8; 9],
    /// VDU 21: output discarded until VDU 6.
    pub(crate) disabled: bool,
    /// VDU 2: characters go to the printer too.
    pub(crate) printing: bool,
    /// The keys have been looked at for the scroll about to happen.
    pub(crate) scroll_cleared: bool,
}

impl Stream {
    pub(crate) const fn new() -> Self {
        Self {
            code: 0,
            gathered: [0; 9],
            disabled: false,
            printing: false,
            scroll_cleared: false,
        }
    }
}

/// Work a byte needs that calls out of the drivers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    /// A character to the printer: `VDU 1`'s own where `vdu_1`.
    Print {
        c: u8,
        vdu_1: bool,
    },
    Bell,
    /// VDU 22.
    Mode(u8),
    /// VDU 19, through `PaletteV`.
    Palette {
        logical: u8,
        physical: u8,
        rgb: [u8; 3],
    },
    /// VDU 20's palette, through `PaletteV`.
    DefaultPalette,
    /// A VDU 23 the drivers do not know, to `UKVDU23V`.
    UnknownVdu23([u8; 9]),
    /// `VDU 23,27`: the system area's sprite named `number` in decimal
    /// selected, by `action` 0, or taken from the screen and selected, by 1.
    Sprite {
        action: u8,
        number: u8,
    },
    /// The system area's selected sprite plotted at the graphics cursor in
    /// `ink`'s action.
    SpritePlot {
        ink: Ink,
    },
    /// A plot code the drivers do not draw, to `UKPlotV`.
    UnknownPlot {
        code: u8,
        x: i32,
        y: i32,
    },
    /// The keys looked at before a scroll: Ctrl and Shift together hold
    /// it, and in paged mode a page's end waits for Shift.
    Keys,
}

/// Takes bytes of the VDU stream.
pub(crate) fn write(os: &mut Os<'_>, bytes: &[u8]) -> Result<(), Fail> {
    let mut rest = bytes;
    while !rest.is_empty() {
        let (used, effect) = with(os, |vdu, os| take(vdu, os, rest))?;
        rest = &rest[used..];
        if let Some(effect) = effect {
            perform(os, effect)?;
        }
    }
    Ok(())
}

/// Whether a control sequence's parameters are part gathered.
pub(crate) fn queued(os: &mut Os<'_>) -> bool {
    os.byte_var(var::VDU_QUEUE) != 0
}

/// The bytes the stream takes before one needs calling out for, and that
/// call.
fn take(vdu: &mut Vdu, os: &mut Os<'_>, bytes: &[u8]) -> Result<(usize, Option<Effect>), Fail> {
    for (i, &b) in bytes.iter().enumerate() {
        let waiting = os.byte_var(var::VDU_QUEUE);
        if waiting != 0 {
            let needed = parameters(vdu.stream.code);
            let have = needed - (0u8.wrapping_sub(waiting)).min(needed);
            if let Some(slot) = vdu.stream.gathered.get_mut(usize::from(have)) {
                *slot = b;
            }
            let left = waiting.wrapping_add(1);
            os.set_byte_var(var::VDU_QUEUE, left);
            if left == 0 {
                let effect = act(vdu, os)?;
                if effect.is_some() {
                    return Ok((i + 1, effect));
                }
            }
            continue;
        }
        if vdu.stream.printing && b != 1 && printed(b) {
            vdu.stream.code = b;
            if let Some(effect) = visible(vdu, os, b)? {
                return Ok((i + 1, Some(effect)));
            }
            return Ok((i + 1, Some(Effect::Print { c: b, vdu_1: false })));
        }
        match Control::of(b) {
            Some(control) if control.parameters() > 0 => {
                vdu.stream.code = b;
                vdu.stream.gathered = [0; 9];
                os.set_byte_var(var::VDU_QUEUE, 0u8.wrapping_sub(control.parameters()));
            }
            _ => {
                vdu.stream.code = b;
                match visible(vdu, os, b) {
                    Ok(Some(Effect::Keys)) => return Ok((i, Some(Effect::Keys))),
                    Ok(Some(effect)) => return Ok((i + 1, Some(effect))),
                    Ok(None) => {}
                    Err(fail) => return Err(fail),
                }
            }
        }
    }
    Ok((bytes.len(), None))
}

/// The parameters code `code` gathers.
pub(crate) fn parameters(code: u8) -> u8 {
    Control::of(code).map_or(0, Control::parameters)
}

/// Whether VDU 2 sends `b` to the printer: printable characters and the
/// codes that move a print head.
const fn printed(b: u8) -> bool {
    matches!(b, 8..=13 | 32..=126 | 128..=255)
}

/// A byte that is a character or a code with no parameters, acted on; a
/// byte that would scroll before the keys are looked at is left to be
/// taken again.
fn visible(vdu: &mut Vdu, os: &mut Os<'_>, b: u8) -> Result<Option<Effect>, Fail> {
    if vdu.stream.disabled && b != 6 {
        return Ok(None);
    }
    let needs = match b {
        10 => vdu.needs_before_newline(),
        9 => {
            let (width, _) = vdu.text.extent();
            if vdu.text.x + 1 >= width {
                vdu.needs_before_newline()
            } else {
                Needs::Nothing
            }
        }
        32..=126 | 128..=255 => vdu.needs_before_print(),
        _ => Needs::Nothing,
    };
    if needs == Needs::KeysBeforeScroll {
        return Ok(Some(Effect::Keys));
    }
    vdu.stream.scroll_cleared = false;
    match b {
        0 | 27 => {}
        2 => vdu.stream.printing = true,
        3 => vdu.stream.printing = false,
        4 => {
            if vdu.text.vdu5 {
                vdu.text.cursor.shown = true;
            }
            vdu.text.vdu5 = false;
        }
        5 => {
            if vdu.format().graphic() {
                vdu.text.vdu5 = true;
            }
        }
        6 => vdu.stream.disabled = false,
        7 => return Ok(Some(Effect::Bell)),
        8 => vdu.back(os)?,
        9 => vdu.forward(os)?,
        10 => vdu.line_down(os)?,
        11 => vdu.line_up(os)?,
        12 => {
            if vdu.text.vdu5 {
                vdu.clear_graphics(os)?;
                vdu.vdu5_home();
            } else {
                vdu.clear_text_window(os)?;
            }
        }
        13 => vdu.carriage_return(),
        14 => {
            vdu.text.paged = true;
            os.set_byte_var(var::PAGE_LINES, 0);
        }
        15 => vdu.text.paged = false,
        16 => vdu.clear_graphics(os)?,
        20 => {
            let format = *vdu.format();
            vdu.colours.defaults(&format);
            vdu.colours.publish(os, &format)?;
            return Ok(Some(Effect::DefaultPalette));
        }
        21 => vdu.stream.disabled = true,
        26 => vdu.default_windows(),
        30 => vdu.home(),
        127 => vdu.delete(os)?,
        _ => vdu.print(os, b)?,
    }
    Ok(None)
}

/// A control code whose parameters are all gathered, acted on.
fn act(vdu: &mut Vdu, os: &mut Os<'_>) -> Result<Option<Effect>, Fail> {
    let q = vdu.stream.gathered;
    let code = vdu.stream.code;
    if vdu.stream.disabled {
        return Ok(None);
    }
    let word = |i: usize| i16::from_le_bytes([q[i], q[i + 1]]);
    match code {
        1 => {
            return Ok(vdu.stream.printing.then_some(Effect::Print {
                c: q[0],
                vdu_1: true,
            }))
        }
        17 => {
            let format = *vdu.format();
            vdu.colours.text_colour(&format, q[0]);
        }
        18 => {
            let format = *vdu.format();
            vdu.colours.graphics_colour(&format, q[0], q[1]);
            vdu.colours.publish(os, &format)?;
        }
        19 => {
            return Ok(Some(Effect::Palette {
                logical: q[0],
                physical: q[1],
                rgb: [q[2], q[3], q[4]],
            }))
        }
        22 => return Ok(Some(Effect::Mode(q[0]))),
        23 => return vdu.vdu_23(os, q),
        24 => vdu.set_graphics_window(
            i32::from(word(0)),
            i32::from(word(2)),
            i32::from(word(4)),
            i32::from(word(6)),
        ),
        25 => return vdu.plot(os, q[0], word(1), word(3)),
        28 => vdu.set_text_window(q[0], q[1], q[2], q[3]),
        29 => vdu.graphics.origin = (i32::from(word(0)), i32::from(word(2))),
        31 => vdu.tab(q[0], q[1]),
        _ => {}
    }
    Ok(None)
}

impl Vdu {
    /// `VDU 28`: the text window, refused where off the screen or inside
    /// out; the cursor home, and copy-cursor editing over.
    pub(crate) fn set_text_window(&mut self, left: u8, bottom: u8, right: u8, top: u8) {
        let format = self.format();
        let (l, b, r, t) = (
            u32::from(left),
            u32::from(bottom),
            u32::from(right),
            u32::from(top),
        );
        let (columns, rows) = format.cells();
        if r >= columns || b >= rows || l > r || t > b {
            return;
        }
        self.text.window = crate::text::Window {
            left: l,
            bottom: b,
            right: r,
            top: t,
        };
        self.text.windowed = true;
        self.text.x = 0;
        self.text.y = 0;
        self.text.editing = None;
    }

    /// `VDU 26`: both windows the whole screen, the origin and graphics
    /// cursor at its corner, the text cursor home; scrolling moves the screen
    /// again.
    pub(crate) fn default_windows(&mut self) {
        let format = *self.format();
        let (columns, rows) = format.cells();
        self.text.window = crate::text::Window {
            left: 0,
            bottom: rows - 1,
            right: columns - 1,
            top: 0,
        };
        self.text.windowed = false;
        self.text.editing = None;
        let chars = (self.graphics.char_size, self.graphics.char_space);
        self.graphics = crate::plot::Graphics::for_screen(format.width(), format.height());
        (self.graphics.char_size, self.graphics.char_space) = chars;
        self.home();
    }

    /// `VDU 23`: an operation by its first parameter, or a character
    /// defined.
    fn vdu_23(&mut self, os: &mut Os<'_>, q: [u8; 9]) -> Result<Option<Effect>, Fail> {
        let format = *self.format();
        let rows = [q[1], q[2], q[3], q[4], q[5], q[6], q[7], q[8]];
        let Some(operation) = Operation::of(q[0]) else {
            if q[0] < FIRST_DEFINED {
                return Ok(Some(Effect::UnknownVdu23(q)));
            }
            self.font.define(q[0], rows);
            return Ok(None);
        };
        match operation {
            Operation::Controller => self.vdu_23_0(q),
            Operation::Cursor => match q[1] {
                0 => self.text.cursor.shown = false,
                1 => self.text.cursor.shown = true,
                2 => self.text.cursor.appearance = 0,
                3 => self.text.cursor.appearance = 2,
                _ => {}
            },
            Operation::Pattern1
            | Operation::Pattern2
            | Operation::Pattern3
            | Operation::Pattern4 => {
                let pattern = usize::from(q[0] - Operation::Pattern1 as u8);
                self.colours.define_pattern(&format, pattern, rows);
                self.colours.publish(os, &format)?;
            }
            Operation::LinePattern => {
                self.colours.dot = u64::from_be_bytes(rows);
                self.colours.dot_at = 0;
            }
            Operation::Scroll => self.scroll_23_7(os, q[1], q[2], q[3])?,
            Operation::ClearBlock => self.clear_block(
                os,
                q[1],
                q[2],
                (q[3].cast_signed(), q[4].cast_signed()),
                (q[5].cast_signed(), q[6].cast_signed()),
            )?,
            Operation::FirstFlash => os.set_byte_var(var::FIRST_DURATION, q[1]),
            Operation::SecondFlash => os.set_byte_var(var::SECOND_DURATION, q[1]),
            Operation::DefaultPatterns => {
                self.colours.default_patterns(&format);
                self.colours.publish(os, &format)?;
            }
            Operation::SimplePattern1
            | Operation::SimplePattern2
            | Operation::SimplePattern3
            | Operation::SimplePattern4 => {
                let pattern = usize::from(q[0] - Operation::SimplePattern1 as u8);
                self.colours.define_simple(&format, pattern, rows);
                self.colours.publish(os, &format)?;
            }
            Operation::CursorMovement => {
                let flags = (self.text.flags & q[2]) ^ q[1];
                self.text.set_flags(flags);
            }
            Operation::Colours => return self.vdu_23_17(os, q).map(|()| None),
            Operation::Teletext => self.teletext_control(os, q[1], q[2])?,
            Operation::Sprites => {
                return Ok(Some(Effect::Sprite {
                    action: q[1],
                    number: q[2],
                }))
            }
        }
        Ok(None)
    }

    /// `VDU 23,0`: the cursor's shape and the interlace, by register.
    fn vdu_23_0(&mut self, q: [u8; 9]) {
        match q[1] {
            10 => {
                self.text.cursor.start = q[2] & 0x1F;
                self.text.cursor.appearance = (q[2] >> 5) & 3;
            }
            11 => self.text.cursor.end = q[2] & 0x1F,
            _ => {}
        }
    }

    /// `VDU 23,17`: tints, the ECF form, the text colours exchanged, the ECF
    /// origin and VDU 5's character size.
    fn vdu_23_17(&mut self, os: &mut Os<'_>, q: [u8; 9]) -> Result<(), Fail> {
        let format = *self.format();
        let word = |i: usize| i16::from_le_bytes([q[i], q[i + 1]]);
        match q[1] {
            0..=3 => self.colours.tint(q[1], q[2]),
            4 => self.colours.native = q[2] & 1 != 0,
            5 => self.colours.swap_text(),
            6 => {
                self.colours.ecf_origin = self.pixels_of(i32::from(word(2)), i32::from(word(4)));
            }
            7 => {
                let (x, y) = (
                    u32::from(word(3).unsigned_abs()),
                    u32::from(word(5).unsigned_abs()),
                );
                if q[2] & 2 != 0 {
                    self.graphics.char_size = (x, y);
                }
                if q[2] & 4 != 0 {
                    self.graphics.char_space = (x, y);
                }
                return Ok(());
            }
            _ => return Ok(()),
        }
        self.colours.publish(os, &format)
    }
}

/// Performs `effect`, the drivers' state not held.
fn perform(os: &mut Os<'_>, effect: Effect) -> Result<(), Fail> {
    match effect {
        Effect::Print { c, vdu_1 } => os.vdu_print(c, vdu_1),
        Effect::Bell => {
            let mut sound = Regs::new([
                u32::from(os.byte_var(var::BELL_CHANNEL)),
                u32::from(os.byte_var(var::BELL_VOLUME)),
                u32::from(os.byte_var(var::BELL_PITCH)),
                u32::from(os.byte_var(var::BELL_DURATION)),
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
            // A machine with no sound modules rings no bell.
            let _ = os.perform(
                tairix_riscos_kernel::swi::X | tairix_riscos_kernel::swi::sound::CONTROL,
                &mut sound,
            );
            Ok(())
        }
        Effect::Mode(mode) => crate::select::vdu_22(os, mode),
        Effect::Palette {
            logical,
            physical,
            rgb,
        } => crate::palette::write_through(os, logical, physical, rgb),
        Effect::DefaultPalette => crate::palette::default_through(os),
        Effect::UnknownVdu23(q) => {
            let block = os.scratch_text(&q)?;
            let mut regs = Regs::new([u32::from(q[0]), block, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            os.call_vector(UK_VDU23, &mut regs, Entry::Supervisor)?;
            Ok(())
        }
        Effect::UnknownPlot { code, x, y } => {
            let mut regs = Regs::new([
                u32::from(code),
                x.cast_unsigned(),
                y.cast_unsigned(),
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
            os.call_vector(UK_PLOT, &mut regs, Entry::Supervisor)?;
            Ok(())
        }
        Effect::Sprite { action, number } => crate::sprite::vdu_23_27(os, action, number),
        Effect::SpritePlot { ink } => crate::sprite::plot_selected(os, ink),
        Effect::Keys => keys_before_scroll(os),
    }
}

/// The keys before a scroll: Ctrl with Shift holds it until released; in
/// paged mode, at a page's end it waits for Shift. Escape ends either wait.
fn keys_before_scroll(os: &mut Os<'_>) -> Result<(), Fail> {
    let paged = with(os, |vdu, os| {
        Ok(vdu.text.paged && u32::from(os.byte_var(var::PAGE_LINES)) >= vdu.page_lines())
    })?;
    loop {
        if os.escape_set() {
            break;
        }
        let shift = key_down(os, 0)?;
        let ctrl = key_down(os, 1)?;
        let held = shift && ctrl;
        if !held && (!paged || shift) {
            break;
        }
        os.await_input()?;
    }
    with(os, |vdu, os| {
        if paged {
            os.set_byte_var(var::PAGE_LINES, 0);
        }
        vdu.stream.scroll_cleared = true;
        Ok(())
    })
}

/// Whether internal key `key` is down: 0 Shift, 1 Ctrl.
fn key_down(os: &mut Os<'_>, key: u32) -> Result<bool, Fail> {
    let mut regs = Regs::new([129, (key ^ 0xFF), 0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    os.perform(
        tairix_riscos_kernel::swi::X | tairix_riscos_kernel::swi::os::BYTE,
        &mut regs,
    )?;
    Ok(regs.r[1] == 0xFF)
}
