//! `MODE`'s display and console: a display mode set as DOS 4's `MODE` sets
//! it, the picture shifted, and the console's columns, lines and typing
//! rate. Columns and lines are set through `ANSI.SYS`'s display information
//! when it is there.

use super::{at, fail, handler, requested, tables, text, Adjust, Display, ModeResident, Request};
use crate::bios::layout::{ADAPTER_ROMS, OPTION_ROM_SIGNATURE, VIDEO_ROM};
use crate::dos::device::{self, generic as ioctl, generic::display};
use crate::dos::message::decimal;
use crate::dos::program::{
    self, answer, say, Flow, Frame, Id, Int, Key, Program, Returned, Routine, ENDED,
};
use crate::dos::shell::set_asciz;
use crate::frames::{locals, take, Fault, Outcome, GENERAL_FAILURE};
use alloc::vec::Vec;

/// INT 2Fh's question to `ANSI.SYS`, and its answer that it is there.
pub const ANSI_CHECK: u16 = 0x1A00;
const INSTALLED: u8 = 0xFF;

/// Whether INT 2Fh's answer to [`ANSI_CHECK`] says `ANSI.SYS` is there.
#[must_use]
pub fn ansi_answered(child: Outcome) -> bool {
    matches!(child, Ok(ax) if ax.to_le_bytes()[0] == INSTALLED)
}

/// The extended errors a console's display information fails with: a mode
/// the hardware cannot show, and a font not loaded.
const NOT_SUPPORTED: u16 = device::extended(display::NOT_SUPPORTED as u16);
const FONT_MISSING: u16 = device::extended(display::FONT_MISSING as u16);

/// What `MODE` says of `ANSI.SYS` refusing `what`: nothing for a failure it
/// gives no words to.
fn refused(fault: Fault, what: &[u8]) -> Vec<u8> {
    match fault {
        Fault::Error(NOT_SUPPORTED) => text::unsupported(what),
        Fault::Error(FONT_MISSING) => text::NO_FONT.to_vec(),
        _ => Vec::new(),
    }
}

locals! {
    /// The console's display information asked of `ANSI.SYS`, or given it,
    /// at [`at::PACKET`]: `CON` opened, the generic IOCTL made, `CON`
    /// closed.
    pub struct ModeAnsi {
        set: bool,
        handle: u16,
        error: u16,
    }
}

mod ansi {
    pub const OPENED: u8 = 1;
    pub const ASKED: u8 = 2;
    pub const CLOSED: u8 = 3;
}

impl Routine for ModeAnsi {
    const ID: Id = Id::ModeAnsi;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                p.set16(at::PACKET + display::LEVEL, 0);
                p.set16(at::PACKET + display::LENGTH, display::DATA);
                set_asciz(p, at::NAME, b"CON", 4);
                *phase = ansi::OPENED;
                Flow::Call(Frame::of(&Int {
                    dx: at::NAME,
                    ..Int::dos(p.own, 0x3D02)
                }))
            }
            ansi::OPENED => {
                self.handle = u16::try_from(take!(child)).unwrap_or(0);
                let function = if self.set {
                    ioctl::SET_DISPLAY
                } else {
                    ioctl::GET_DISPLAY
                };
                *phase = ansi::ASKED;
                Flow::Call(Frame::of(&Int {
                    bx: self.handle,
                    cx: u16::from_le_bytes([function, ioctl::DISPLAY]),
                    dx: at::PACKET,
                    ..Int::dos(p.own, 0x440C)
                }))
            }
            ansi::ASKED => {
                self.error = match child {
                    Ok(_) => 0,
                    Err(Fault::Error(error)) => error,
                    Err(_) => GENERAL_FAILURE,
                };
                *phase = ansi::CLOSED;
                Flow::Call(Frame::of(&Int {
                    bx: self.handle,
                    ..Int::dos(p.own, 0x3E00)
                }))
            }
            _ => Flow::Return(if self.error == 0 {
                Ok(0)
            } else {
                Err(Fault::Error(self.error))
            }),
        }
    }
}

/// The display codes the BIOS names adapters and monitors by, as INT 10h
/// function `1Ah` gives them; and one an adapter showing every mode is
/// counted as.
mod code {
    pub const NONE: u8 = 0x00;
    pub const MDA: u8 = 0x01;
    pub const CGA: u8 = 0x02;
    pub const EGA_COLOUR: u8 = 0x04;
    pub const EGA_MONO: u8 = 0x05;
    pub const ALL: u8 = 0xFE;
    /// Those that show colour modes, and monochrome ones.
    pub const COLOUR: [u8; 7] = [0x0B, 0x06, 0x08, 0x0A, 0x0C, CGA, EGA_COLOUR];
    pub const MONOCHROME: [u8; 3] = [MDA, EGA_MONO, 0x07];
}

/// Whether video memory answers at `segment`: a byte written there reads
/// back, and is put back as it was.
fn answers(p: &mut Program<'_, '_>, segment: u16) -> bool {
    let kept = p.g.read8(segment, 0);
    p.g.write8(segment, 0, 0x55);
    let back = p.g.read8(segment, 0);
    p.g.write8(segment, 0, kept);
    back == 0x55
}

fn packed(active: u8, alternate: u8) -> u32 {
    u32::from(u16::from_le_bytes([active, alternate]))
}

locals! {
    /// The displays the machine has, as DOS 4's `MODE` finds them: the VGA
    /// BIOS's state, else the EGA's, else which adapters' memory answers.
    /// Gives the active display's code and the alternate's.
    pub struct ModeDisplays {}
}

mod displays {
    pub const QUERIED: u8 = 1;
    pub const EGA: u8 = 2;
}

impl ModeDisplays {
    /// With no EGA or VGA: a monochrome adapter is the active display, a
    /// CGA the alternate.
    fn plain(p: &mut Program<'_, '_>) -> u32 {
        let active = if answers(p, 0xB000) {
            code::MDA
        } else {
            code::NONE
        };
        let alternate = if answers(p, 0xB800) {
            code::CGA
        } else {
            code::NONE
        };
        packed(active, alternate)
    }
}

impl Routine for ModeDisplays {
    const ID: Id = Id::ModeDisplays;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                *phase = displays::QUERIED;
                Flow::Call(Frame::of(&Int {
                    di: at::VIDEO_STATE,
                    ..Int::bios(p.own, 0x10, 0x1B00)
                }))
            }
            displays::QUERIED => {
                if take!(child).to_le_bytes()[0] == 0x1B {
                    let (mut active, alternate) = (
                        p.get8(at::VIDEO_STATE + 0x25),
                        p.get8(at::VIDEO_STATE + 0x26),
                    );
                    if alternate == code::NONE && p.get8(at::VIDEO_STATE + 0x2D) & 0x01 != 0 {
                        active = code::ALL;
                    }
                    return Flow::Return(Ok(packed(active, alternate)));
                }
                if p.g.read16(VIDEO_ROM, 0) != OPTION_ROM_SIGNATURE {
                    return Flow::Return(Ok(Self::plain(p)));
                }
                *phase = displays::EGA;
                Flow::Call(Frame::of(&Int {
                    bx: 0x0010,
                    ..Int::bios(p.own, 0x10, 0x1200)
                }))
            }
            _ => {
                take!(child);
                let [bl, bh] = p.result(Returned::Bx).to_le_bytes();
                if bl == 0x10 {
                    return Flow::Return(Ok(Self::plain(p)));
                }
                Flow::Return(Ok(if bh == 0 {
                    let alternate = if answers(p, 0xB000) {
                        code::MDA
                    } else {
                        code::NONE
                    };
                    packed(code::EGA_COLOUR, alternate)
                } else {
                    let alternate = if answers(p, 0xB800) {
                        code::CGA
                    } else {
                        code::NONE
                    };
                    packed(code::EGA_MONO, alternate)
                }))
            }
        }
    }
}

/// What a display mode sets: the equipment word's video bits, the BIOS's
/// mode, the cursor, and whether it is a colour mode.
fn setup(display: Display) -> (u16, u8, u16, bool) {
    match display {
        Display::Bw40 => (0x10, 0, 0x0607, true),
        Display::Co40 | Display::Forty => (0x10, 1, 0x0607, true),
        Display::Bw80 => (0x20, 2, 0x0607, true),
        Display::Co80 | Display::Eighty => (0x20, 3, 0x0607, true),
        Display::Mono => (0x30, 7, 0x0B0C, false),
    }
}

/// The mode `40` or `80` turns mode `current` into: the same colours at the
/// columns asked for.
fn columns_mode(current: u8, forty: bool) -> u8 {
    match current {
        0 | 2 | 5 | 6 | 7 | 0x11 => {
            if forty {
                0
            } else {
                2
            }
        }
        1 | 3 | 4 | 0x12 | 0x13 => {
            if forty {
                1
            } else {
                3
            }
        }
        other => other,
    }
}

const EQUIPMENT_VIDEO: u16 = 0x30;

locals! {
    /// A display mode set: `40` and `80` through `ANSI.SYS` when it is
    /// there, else as the mode in force at those columns; the others on a
    /// display that shows them, the equipment word set to match, the cursor
    /// home and shaped. Gives `1` once it has said an error.
    pub struct ModeDisplay {
        display: u8,
        ansi: bool,
    }
}

mod setting {
    pub const CURRENT: u8 = 1;
    pub const ANSI_SET: u8 = 2;
    pub const DETECTED: u8 = 3;
    pub const SET: u8 = 4;
    pub const PLACED: u8 = 5;
    pub const DONE: u8 = 6;
    pub const FAILED: u8 = 7;
}

impl Routine for ModeDisplay {
    const ID: Id = Id::ModeDisplay;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let Some(display) = Display::of(self.display) else {
            return Flow::Return(Ok(1));
        };
        let (bits, mode, cursor, colour) = setup(display);
        let forty = display == Display::Forty;
        match *phase {
            0 if matches!(display, Display::Forty | Display::Eighty) => {
                if self.ansi {
                    p.set16(at::PACKET + display::COLUMNS, if forty { 40 } else { 80 });
                    *phase = setting::ANSI_SET;
                    return Flow::Call(Frame::of(&ModeAnsi {
                        set: true,
                        ..ModeAnsi::default()
                    }));
                }
                *phase = setting::CURRENT;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x10, 0x0F00)))
            }
            0 => {
                *phase = setting::DETECTED;
                Flow::Call(Frame::of(&ModeDisplays {}))
            }
            setting::CURRENT => {
                let current = take!(child).to_le_bytes()[0] & 0x7F;
                *phase = setting::DONE;
                Flow::Call(Frame::of(&Int::bios(
                    p.own,
                    0x10,
                    u16::from(columns_mode(current, forty)),
                )))
            }
            setting::ANSI_SET => match child {
                Ok(_) => Flow::Return(Ok(0)),
                Err(fault) => {
                    fail(p);
                    let said = refused(fault, display.name());
                    *phase = setting::FAILED;
                    if said.is_empty() {
                        return self.step(p, phase, Ok(0));
                    }
                    Flow::Call(say(p, &said))
                }
            },
            setting::DETECTED => {
                let [active, alternate, ..] = take!(child).to_le_bytes();
                let shows = if colour {
                    &code::COLOUR[..]
                } else {
                    &code::MONOCHROME[..]
                };
                if active != code::ALL && !shows.contains(&active) && !shows.contains(&alternate) {
                    fail(p);
                    *phase = setting::FAILED;
                    let said = text::unsupported(display.name());
                    return Flow::Call(say(p, &said));
                }
                let equipment =
                    p.g.read16(crate::bios::bda::SEGMENT, crate::bios::bda::EQUIPMENT);
                p.g.write16(
                    crate::bios::bda::SEGMENT,
                    crate::bios::bda::EQUIPMENT,
                    equipment & !EQUIPMENT_VIDEO | bits,
                );
                *phase = setting::SET;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x10, u16::from(mode))))
            }
            setting::SET => {
                take!(child);
                *phase = setting::PLACED;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x10, 0x0200)))
            }
            setting::PLACED => {
                take!(child);
                *phase = setting::DONE;
                Flow::Call(Frame::of(&Int {
                    cx: cursor,
                    ..Int::bios(p.own, 0x10, 0x0100)
                }))
            }
            setting::DONE => {
                take!(child);
                Flow::Return(Ok(0))
            }
            _ => Flow::Return(Ok(1)),
        }
    }
}

/// The video parameter table's horizontal sync positions, the 6845's R2, of
/// the 40-column, 80-column and graphics modes; a shift moves the 80-column
/// one by two.
const SYNC_40: u16 = 0x02;
const SYNC_80: u16 = 0x12;
const SYNC_GRAPHICS: u16 = 0x22;
/// The farthest the 40-column sync is moved: right to 1, left to 2Dh.
const RIGHTMOST: u8 = 0x01;
const LEFTMOST: u8 = 0x2E;

fn cant(right: bool) -> Vec<u8> {
    let way: &[u8] = if right { b"right" } else { b"left" };
    [&b"\r\n      Unable to shift screen "[..], way, b"\r\n"].concat()
}

fn question(right: bool) -> Vec<u8> {
    let end: &[u8] = if right { b"leftmost 0" } else { b"rightmost 9" };
    [&b"\r\nDo you see the "[..], end, b"? (YES/NO)\r\n"].concat()
}

locals! {
    /// The picture shifted a character right or left on a CGA, through the
    /// video parameter table: the ROM's copied into the resident part the
    /// first time, then its horizontal syncs moved and the mode set again.
    /// With a test pattern, it asks whether the edge shows and shifts again
    /// until it does.
    pub struct ModeShift {
        right: bool,
        test: bool,
        mode: u8,
    }
}

mod shift {
    pub const CURRENT: u8 = 1;
    pub const EGA: u8 = 2;
    pub const RESIDENT: u8 = 3;
    pub const GUARDED: u8 = 4;
    pub const MOVED: u8 = 5;
    pub const RESET: u8 = 6;
    pub const CLEARED: u8 = 7;
    pub const SHOWN: u8 = 8;
    pub const ASKED: u8 = 9;
    pub const ANSWERED: u8 = 10;
    pub const CHECKED: u8 = 11;
    pub const DONE: u8 = 12;
}

impl ModeShift {
    /// The table INT 1Dh leads to, copied from ROM into `segment`'s tables
    /// and INT 1Dh led there.
    fn take_table(p: &mut Program<'_, '_>, segment: u16) {
        let (offset, rom) = (p.g.read16(0, 0x1D * 4), p.g.read16(0, 0x1D * 4 + 2));
        let mut bytes = [0u8; at::VIDEO_TABLE_LEN as usize];
        p.g.read_bytes(rom, offset, &mut bytes);
        p.g.write_bytes(segment, at::VIDEO_TABLE, &bytes);
        p.g.write16(0, 0x1D * 4, at::VIDEO_TABLE);
        p.g.write16(0, 0x1D * 4 + 2, segment);
    }

    /// The table in RAM: copied into the tables there are, or into this
    /// copy made the resident part.
    fn find_table(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if p.g.read16(0, 0x1D * 4 + 2) >= ADAPTER_ROMS {
            let Some(segment) = tables(p) else {
                *phase = shift::RESIDENT;
                return Flow::Call(Frame::of(&ModeResident {}));
            };
            Self::take_table(p, segment);
        }
        self.move_sync(p, phase)
    }

    /// The syncs moved a character, if they are not as far as they go.
    fn move_sync(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let (offset, segment) = (p.g.read16(0, 0x1D * 4), p.g.read16(0, 0x1D * 4 + 2));
        let sync = p.g.read8(segment, offset.wrapping_add(SYNC_40));
        let room = if self.right {
            sync > RIGHTMOST
        } else {
            sync < LEFTMOST
        };
        if !room {
            *phase = shift::DONE;
            let said = cant(self.right);
            return Flow::Call(say(p, &said));
        }
        for (at, by) in [(SYNC_40, 1u8), (SYNC_80, 2), (SYNC_GRAPHICS, 1)] {
            let at = offset.wrapping_add(at);
            let value = p.g.read8(segment, at);
            let moved = if self.right {
                value.wrapping_sub(by)
            } else {
                value.wrapping_add(by)
            };
            p.g.write8(segment, at, moved);
        }
        *phase = shift::MOVED;
        Flow::Call(Frame::of(&Int::bios(p.own, 0x10, 0x0F00)))
    }

    /// The screen cleared in the test's mode, for the pattern.
    fn clear(self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = shift::CLEARED;
        Flow::Call(Frame::of(&Int::bios(p.own, 0x10, u16::from(self.mode))))
    }
}

impl Routine for ModeShift {
    const ID: Id = Id::ModeShift;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                *phase = shift::CURRENT;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x10, 0x0F00)))
            }
            // A monochrome mode, or an EGA with a colour monitor, cannot be
            // shifted.
            shift::CURRENT => {
                if take!(child).to_le_bytes()[0] >= 7 {
                    *phase = shift::DONE;
                    let said = cant(self.right);
                    return Flow::Call(say(p, &said));
                }
                if p.g.read16(VIDEO_ROM, 0) != OPTION_ROM_SIGNATURE {
                    return self.find_table(p, phase);
                }
                *phase = shift::EGA;
                Flow::Call(Frame::of(&Int {
                    bx: 0xFF10,
                    ..Int::bios(p.own, 0x10, 0x1200)
                }))
            }
            shift::EGA => {
                take!(child);
                if p.result(Returned::Bx).to_le_bytes()[1] == 0 {
                    *phase = shift::DONE;
                    let said = cant(self.right);
                    return Flow::Call(say(p, &said));
                }
                self.find_table(p, phase)
            }
            // This copy is to be the resident part: Ctrl+C from here on ends
            // its work with the copy staying, so the vector it is about to
            // lead to its table never outlives it.
            shift::RESIDENT => {
                take!(child);
                *phase = shift::GUARDED;
                Flow::Call(Frame::of(&Int {
                    dx: program::handler_entry(handler::BREAK),
                    ..Int::dos(p.own, 0x2523)
                }))
            }
            shift::GUARDED => {
                take!(child);
                Self::take_table(p, p.own);
                self.move_sync(p, phase)
            }
            shift::MOVED => {
                self.mode = take!(child).to_le_bytes()[0];
                *phase = shift::RESET;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x10, u16::from(self.mode))))
            }
            // DOS's test pattern is shown in the colour mode of the
            // columns, as its `MODE` shows it.
            shift::RESET => {
                take!(child);
                if !self.test {
                    return Flow::Return(Ok(0));
                }
                self.mode |= 1;
                self.clear(p, phase)
            }
            shift::CLEARED => {
                take!(child);
                let copies = if self.mode == 1 { 4 } else { 8 };
                let pattern = b"0123456789".repeat(copies);
                *phase = shift::SHOWN;
                Flow::Call(say(p, &pattern))
            }
            shift::SHOWN => {
                take!(child);
                *phase = shift::ASKED;
                let asked = question(self.right);
                Flow::Call(say(p, &asked))
            }
            shift::ASKED => {
                take!(child);
                *phase = shift::ANSWERED;
                Flow::Call(Frame::of(&Key::of(0x0C01)))
            }
            // An input with no answer left takes the display as it stands.
            shift::ANSWERED => {
                let key = take!(child);
                if key == ENDED {
                    return Flow::Return(Ok(0));
                }
                *phase = shift::CHECKED;
                Flow::Call(Frame::of(&Int {
                    dx: u16::try_from(key).unwrap_or(0),
                    ..Int::dos(p.own, 0x6523)
                }))
            }
            shift::CHECKED => match take!(child) {
                answer::YES => Flow::Return(Ok(0)),
                answer::NO => {
                    *phase = 0;
                    self.step(p, phase, Ok(0))
                }
                _ => self.clear(p, phase),
            },
            _ => {
                take!(child);
                Flow::Return(Ok(0))
            }
        }
    }
}

locals! {
    /// A display or console request: whether `ANSI.SYS` is there asked
    /// first, and its display information read, as DOS 4's `MODE` does;
    /// then the display mode, the shift or the rows, or the console's
    /// columns, lines and typing rate.
    pub struct ModeVideo {
        ansi: bool,
    }
}

mod stage {
    pub const CHECKED: u8 = 1;
    pub const READ: u8 = 2;
    pub const DISPLAYED: u8 = 3;
    pub const ROWS_SET: u8 = 4;
    pub const SAID: u8 = 5;
    pub const CONSOLE_SET: u8 = 6;
    pub const CONSOLE_SAID: u8 = 7;
    pub const TYPED: u8 = 8;
}

impl ModeVideo {
    fn begin(self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        match requested(p) {
            Some(Request::Display {
                display: Some(display),
                ..
            }) => {
                *phase = stage::DISPLAYED;
                Flow::Call(Frame::of(&ModeDisplay {
                    display: display as u8,
                    ansi: self.ansi,
                }))
            }
            Some(Request::Display {
                display: None,
                adjust,
            }) => self.adjust(p, phase, adjust),
            Some(Request::Console {
                columns,
                lines,
                typematic,
            }) => self.console(p, phase, columns, lines, typematic),
            _ => Flow::Return(Ok(0)),
        }
    }

    fn adjust(self, p: &mut Program<'_, '_>, phase: &mut u8, adjust: Option<Adjust>) -> Flow {
        match adjust {
            None => Flow::Return(Ok(0)),
            Some(Adjust::Shift { right, test }) => {
                *phase = stage::SAID;
                Flow::Call(Frame::of(&ModeShift {
                    right,
                    test,
                    mode: 0,
                }))
            }
            Some(Adjust::Rows(rows)) if self.ansi => {
                p.set16(at::PACKET + display::ROWS, u16::from(rows));
                *phase = stage::ROWS_SET;
                Flow::Call(Frame::of(&ModeAnsi {
                    set: true,
                    ..ModeAnsi::default()
                }))
            }
            Some(Adjust::Rows(_)) => {
                fail(p);
                *phase = stage::SAID;
                Flow::Call(say(p, text::NO_ANSI))
            }
        }
    }

    /// The console's lines, and its columns, through `ANSI.SYS`; without it,
    /// columns as `40` and `80` set them and lines refused.
    fn console(
        self,
        p: &mut Program<'_, '_>,
        phase: &mut u8,
        columns: Option<u8>,
        lines: Option<u8>,
        typematic: Option<(u8, u8)>,
    ) -> Flow {
        if let Some(lines) = lines {
            p.set16(at::PACKET + display::ROWS, u16::from(lines));
        }
        if let (Some(columns), true) = (columns, self.ansi) {
            p.set16(at::PACKET + display::COLUMNS, u16::from(columns));
        }
        if lines.is_some() || (columns.is_some() && self.ansi) {
            if self.ansi {
                *phase = stage::CONSOLE_SET;
                return Flow::Call(Frame::of(&ModeAnsi {
                    set: true,
                    ..ModeAnsi::default()
                }));
            }
            fail(p);
            *phase = stage::CONSOLE_SAID;
            return Flow::Call(say(p, text::NO_ANSI));
        }
        if let Some(columns) = columns {
            *phase = stage::CONSOLE_SAID;
            let display = if columns == 40 {
                Display::Forty
            } else {
                Display::Eighty
            };
            return Flow::Call(Frame::of(&ModeDisplay {
                display: display as u8,
                ansi: false,
            }));
        }
        Self::typematic(p, phase, typematic)
    }

    /// The keyboard's repeat rate and delay set through INT 16h, which a
    /// BIOS without the function ignores.
    fn typematic(p: &mut Program<'_, '_>, phase: &mut u8, typematic: Option<(u8, u8)>) -> Flow {
        let Some((rate, delay)) = typematic else {
            return Flow::Return(Ok(0));
        };
        *phase = stage::TYPED;
        Flow::Call(Frame::of(&Int {
            bx: u16::from_le_bytes([32 - rate, delay - 1]),
            ..Int::bios(p.own, 0x16, 0x0305)
        }))
    }

    fn console_typematic(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        match requested(p) {
            Some(Request::Console { typematic, .. }) => Self::typematic(p, phase, typematic),
            _ => Flow::Return(Ok(0)),
        }
    }

    /// What `MODE` names a console setting `ANSI.SYS` refused by.
    fn console_what(p: &mut Program<'_, '_>) -> Vec<u8> {
        match requested(p) {
            Some(Request::Console {
                lines: Some(lines), ..
            }) => [&b"LINES="[..], &decimal(u32::from(lines))].concat(),
            Some(Request::Console {
                columns: Some(columns),
                ..
            }) => [&b"COLS="[..], &decimal(u32::from(columns))].concat(),
            _ => Vec::new(),
        }
    }
}

impl Routine for ModeVideo {
    const ID: Id = Id::ModeVideo;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                *phase = stage::CHECKED;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x2F, ANSI_CHECK)))
            }
            stage::CHECKED => {
                self.ansi = ansi_answered(child);
                if !self.ansi {
                    return self.begin(p, phase);
                }
                *phase = stage::READ;
                Flow::Call(Frame::of(&ModeAnsi::default()))
            }
            stage::READ => {
                if child.is_ok() {
                    p.set8(at::PACKET + display::MODE, display::TEXT);
                }
                self.begin(p, phase)
            }
            stage::DISPLAYED => {
                if child != Ok(0) {
                    return Flow::Return(Ok(0));
                }
                let adjust = match requested(p) {
                    Some(Request::Display { adjust, .. }) => adjust,
                    _ => None,
                };
                self.adjust(p, phase, adjust)
            }
            stage::ROWS_SET => match child {
                Ok(_) => Flow::Return(Ok(0)),
                Err(fault) => {
                    fail(p);
                    let rows = p.get16(at::PACKET + display::ROWS);
                    let said = refused(fault, &decimal(u32::from(rows)));
                    *phase = stage::SAID;
                    if said.is_empty() {
                        return Flow::Return(Ok(0));
                    }
                    Flow::Call(say(p, &said))
                }
            },
            stage::CONSOLE_SET => match child {
                Ok(_) => Self::console_typematic(p, phase),
                Err(fault) => {
                    fail(p);
                    let what = Self::console_what(p);
                    let said = refused(fault, &what);
                    *phase = stage::CONSOLE_SAID;
                    if said.is_empty() {
                        return Self::console_typematic(p, phase);
                    }
                    Flow::Call(say(p, &said))
                }
            },
            stage::CONSOLE_SAID => Self::console_typematic(p, phase),
            _ => {
                take!(child);
                Flow::Return(Ok(0))
            }
        }
    }
}
