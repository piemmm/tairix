//! `ANSI.SYS`'s display information, generic IOCTL category `03h`: the
//! console's mode, colours, picture and character cells read, and a mode
//! set from them — 43 and 50 lines on the EGA and VGA by their 8×8 font —
//! as DOS 4's `ANSI.SYS` keeps them. The display hardware is found once, at
//! INIT, and the modes it shows follow from it.

use super::ansi::at;
use super::Driver;
use crate::asm::{Reg16, Reg8};
use crate::bios::bda;
use crate::cpu::Seg;
use crate::dos::device::field;
use crate::dos::device::generic::{display, DISPLAY, GET_DISPLAY, SET_DISPLAY};
use crate::dos::engine::Far;
use alloc::vec::Vec;

/// What INIT found, as [`at::HARDWARE`] keeps it: whether a monochrome and
/// a colour adapter's memory answer, and the EGA, by its monitor, or VGA.
pub mod hardware {
    pub const MONO: u8 = 0x01;
    pub const COLOUR: u8 = 0x02;
    pub const KIND: u8 = 0x70;
    pub const EGA_MONO: u8 = 0x10;
    pub const EGA_COLOUR: u8 = 0x20;
    pub const EGA_ENHANCED: u8 = 0x30;
    pub const VGA: u8 = 0x40;
}

/// A mode the hardware shows: its number, text or graphics, its colours,
/// its picture's pixels and its character cells. A length unknown is
/// [`ANY`]; a VGA text mode's rows are, set by its scan lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    mode: u8,
    text: bool,
    colours: u16,
    width: u16,
    height: u16,
    columns: u16,
    rows: u16,
}

const ANY: u16 = 0xFFFF;
const ROWS: u16 = 25;

const fn text(mode: u8, colours: u16, columns: u16, rows: u16) -> Entry {
    Entry {
        mode,
        text: true,
        colours,
        width: ANY,
        height: ANY,
        columns,
        rows,
    }
}

const fn graphics(
    mode: u8,
    colours: u16,
    width: u16,
    height: u16,
    columns: u16,
    rows: u16,
) -> Entry {
    Entry {
        mode,
        text: false,
        colours,
        width,
        height,
        columns,
        rows,
    }
}

/// DOS 4's tables, each colour mode listed before its grey one so that a
/// search finds it first.
const COLOUR_TABLE: [Entry; 7] = [
    text(1, 16, 40, ROWS),
    text(0, 16, 40, ROWS),
    text(3, 16, 80, ROWS),
    text(2, 16, 80, ROWS),
    graphics(4, 4, 320, 200, 40, 25),
    graphics(5, 2, 320, 200, 40, 25),
    graphics(6, 2, 640, 200, 80, 25),
];
const MONO_TABLE: [Entry; 1] = [text(7, 0, 80, ROWS)];
const VGA_TABLE: [Entry; 12] = [
    text(1, 16, 40, ANY),
    text(0, 16, 40, ANY),
    text(3, 16, 80, ANY),
    text(2, 16, 80, ANY),
    text(7, 0, 80, ANY),
    graphics(13, 16, 320, 200, 40, 25),
    graphics(14, 16, 640, 200, 80, 25),
    graphics(15, 0, 640, 350, 80, 25),
    graphics(16, 16, 640, 350, 80, 25),
    graphics(17, 2, 640, 480, 80, 30),
    graphics(18, 16, 640, 480, 80, 30),
    graphics(19, 256, 320, 200, 40, 25),
];
const EGA_ENHANCED_TABLE: [Entry; 7] = [
    text(1, 16, 40, 43),
    text(0, 16, 40, 43),
    text(3, 16, 80, 43),
    text(2, 16, 80, 43),
    graphics(13, 16, 320, 200, 40, 25),
    graphics(14, 16, 640, 200, 80, 25),
    graphics(16, 16, 640, 350, 80, 25),
];
const EGA_COLOUR_TABLE: [Entry; 2] = [
    graphics(13, 16, 320, 200, 40, 25),
    graphics(14, 16, 640, 200, 80, 25),
];
const EGA_MONO_TABLE: [Entry; 2] = [text(7, 0, 80, 43), graphics(15, 0, 640, 350, 80, 25)];

/// The modes `hardware` shows, as DOS 4's `ANSI.SYS` loads its tables: a
/// later table's mode in place of the same mode an earlier one gave.
fn modes(hardware: u8) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::with_capacity(24);
    let mut load = |table: &[Entry]| {
        for entry in table {
            match out.iter_mut().find(|kept| kept.mode == entry.mode) {
                Some(kept) => *kept = *entry,
                None => out.push(*entry),
            }
        }
    };
    if hardware & hardware::MONO != 0 {
        load(&MONO_TABLE);
    }
    if hardware & hardware::COLOUR != 0 {
        load(&COLOUR_TABLE);
    }
    match hardware & hardware::KIND {
        hardware::VGA => {
            load(&COLOUR_TABLE);
            load(&VGA_TABLE);
        }
        hardware::EGA_ENHANCED => {
            load(&COLOUR_TABLE);
            load(&EGA_ENHANCED_TABLE);
        }
        hardware::EGA_COLOUR => {
            load(&COLOUR_TABLE);
            load(&EGA_COLOUR_TABLE);
        }
        hardware::EGA_MONO => load(&EGA_MONO_TABLE),
        _ => {}
    }
    out
}

/// Whether `hardware` is an EGA or VGA: blink is settable, and more than 25
/// lines possible.
const fn planar(hardware: u8) -> bool {
    hardware & hardware::KIND != 0
}

/// The VGA's scan-line setting for `rows` of its 8-pixel font, if it offers
/// it: `200`, `350` and `400` lines are bits 0–2 of what it offers, and
/// settings 0–2 of INT 10h function `12h`'s `30h`.
fn scans_for(rows: u16, offered: u8) -> Option<u8> {
    let setting = match u32::from(rows) * 8 {
        200 => 0,
        344 => 1,
        400 => 2,
        _ => return None,
    };
    (offered & (1 << setting) != 0).then_some(setting)
}

/// The request's record.
struct Record(Far);

impl Record {
    fn byte(&self, d: &mut Driver<'_, '_>, at: u16) -> u8 {
        d.g.read8(self.0.segment, self.0.offset.wrapping_add(at))
    }

    fn word(&self, d: &mut Driver<'_, '_>, at: u16) -> u16 {
        d.g.read16(self.0.segment, self.0.offset.wrapping_add(at))
    }

    fn set_byte(&self, d: &mut Driver<'_, '_>, at: u16, value: u8) {
        d.g.write8(self.0.segment, self.0.offset.wrapping_add(at), value);
    }

    fn set_word(&self, d: &mut Driver<'_, '_>, at: u16, value: u16) {
        d.g.write16(self.0.segment, self.0.offset.wrapping_add(at), value);
    }

    /// The mode it asks for among those `hardware` shows: text by its
    /// columns, graphics by its picture, either by its colours.
    fn wanted(&self, d: &mut Driver<'_, '_>, hardware: u8) -> Option<Entry> {
        let (mode, reserved) = (self.byte(d, display::MODE), self.byte(d, display::MODE + 1));
        let (colours, width, height, columns) = (
            self.word(d, display::COLOURS),
            self.word(d, display::WIDTH),
            self.word(d, display::HEIGHT),
            self.word(d, display::COLUMNS),
        );
        if reserved != 0 || !matches!(mode, display::TEXT | display::GRAPHICS) {
            return None;
        }
        modes(hardware).into_iter().find(|entry| {
            entry.text == (mode == display::TEXT)
                && entry.colours == colours
                && if entry.text {
                    entry.columns == columns
                } else {
                    entry.width == width && entry.height == height
                }
        })
    }
}

/// The phases a request reaches, each after a BIOS call.
mod step {
    pub const GOT_MODE: u8 = 1;
    pub const GOT_STATE: u8 = 2;
    pub const SCANS_SET: u8 = 3;
    pub const MODE_SET: u8 = 4;
    pub const FONT_LOADED: u8 = 5;
    pub const BLOCK_SET: u8 = 6;
    pub const FLAG_SET: u8 = 7;
}

/// `GENERIC_IOCTL`: the console's display information read or set; any
/// other category or function refused.
pub fn serve(d: &mut Driver<'_, '_>) {
    let record = Record(d.get_far(field::PACKET));
    match (d.get8(field::CATEGORY), d.get8(field::FUNCTION)) {
        (DISPLAY, GET_DISPLAY) => get(d, &record),
        (DISPLAY, SET_DISPLAY) => set(d, &record),
        _ => super::unknown(d),
    }
}

fn bios(d: &mut Driver<'_, '_>, ax: u16, bx: u16, then: u8) {
    d.g.set_r16(Reg16::Ax, ax);
    d.g.set_r16(Reg16::Bx, bx);
    d.set_phase(then);
    d.bios(0x10);
}

/// The record filled with the mode in force: its rows the BIOS's own where
/// the mode's are not fixed, and on the VGA its blink as the BIOS reports
/// it.
fn get(d: &mut Driver<'_, '_>, record: &Record) {
    let hardware = d.own8(at::HARDWARE);
    match d.phase() {
        0 => {
            if record.byte(d, display::LEVEL) != 0
                || record.word(d, display::LENGTH) < display::DATA
            {
                return d.finish(Some(display::INVALID));
            }
            bios(d, 0x0F00, 0, step::GOT_MODE);
        }
        step::GOT_MODE => {
            let mode = d.g.r8(Reg8::Al) & 0x7F;
            let Some(entry) = modes(hardware).into_iter().find(|entry| entry.mode == mode) else {
                return d.finish(Some(display::NOT_SUPPORTED));
            };
            let rows = if entry.text && entry.rows != ROWS {
                u16::from(d.g.bda8(bda::ROWS)) + 1
            } else {
                entry.rows
            };
            record.set_byte(d, display::LEVEL + 1, 0);
            record.set_word(d, display::LENGTH, display::DATA);
            record.set_byte(
                d,
                display::MODE,
                if entry.text {
                    display::TEXT
                } else {
                    display::GRAPHICS
                },
            );
            record.set_byte(d, display::MODE + 1, 0);
            for (at, value) in [
                (display::COLOURS, entry.colours),
                (display::WIDTH, entry.width),
                (display::HEIGHT, entry.height),
                (display::COLUMNS, entry.columns),
                (display::ROWS, rows),
            ] {
                record.set_word(d, at, value);
            }
            if hardware & hardware::KIND == hardware::VGA {
                let segment = d.instance();
                d.g.regs_mut().set_sreg(Seg::Es, segment);
                d.g.set_r16(Reg16::Di, at::VIDEO_STATE);
                return bios(d, 0x1B00, 0, step::GOT_STATE);
            }
            let intensity = if planar(hardware) {
                u16::from(d.own8(at::INTENSITY))
            } else {
                0
            };
            record.set_word(d, display::FLAGS, intensity);
            d.finish(None);
        }
        step::GOT_STATE => {
            let blink = d.own8(at::VIDEO_STATE + 0x2D) & 0x20 != 0;
            record.set_word(
                d,
                display::FLAGS,
                if blink { 0 } else { display::INTENSITY },
            );
            d.finish(None);
        }
        _ => d.finish(Some(display::INVALID)),
    }
}

/// The mode the record asks for set: a VGA text mode's scan lines chosen
/// for its rows first, the 8×8 font loaded for more than 25, then on an
/// EGA or VGA the blink it asks for.
fn set(d: &mut Driver<'_, '_>, record: &Record) {
    let hardware = d.own8(at::HARDWARE);
    let flags = record.word(d, display::FLAGS);
    let rows = record.word(d, display::ROWS);
    if d.phase() == 0 {
        let invalid = record.byte(d, display::LEVEL) != 0
            || record.word(d, display::LENGTH) != display::DATA
            || flags & !display::INTENSITY != 0
            || (flags & display::INTENSITY != 0 && !planar(hardware));
        if invalid {
            return d.finish(Some(display::INVALID));
        }
    }
    let Some(entry) = record.wanted(d, hardware) else {
        return d.finish(Some(display::NOT_SUPPORTED));
    };
    match d.phase() {
        0 if !entry.text => set_mode(d, entry, hardware),
        0 if entry.rows == ANY || hardware & hardware::KIND == hardware::VGA => {
            let Some(setting) = scans_for(rows, d.own8(at::SCANS)) else {
                return d.finish(Some(display::NOT_SUPPORTED));
            };
            let setting = if rows == ROWS {
                d.own8(at::MAX_SCANS)
            } else {
                setting
            };
            bios(d, 0x1200 | u16::from(setting), 0x0030, step::SCANS_SET);
        }
        0 => {
            if rows != ROWS && rows != entry.rows {
                return d.finish(Some(display::NOT_SUPPORTED));
            }
            emulate_cursor(d, hardware, rows);
            set_mode(d, entry, hardware);
        }
        step::SCANS_SET => set_mode(d, entry, hardware),
        step::MODE_SET if entry.text && rows != ROWS => bios(d, 0x1112, 0, step::FONT_LOADED),
        step::FONT_LOADED => bios(d, 0x1103, 0, step::BLOCK_SET),
        step::MODE_SET | step::BLOCK_SET if entry.text && planar(hardware) => {
            emulate_cursor(d, hardware, rows);
            let intensity = flags & display::INTENSITY != 0;
            d.set_own8(at::INTENSITY, u8::from(intensity));
            bios(d, 0x1003, u16::from(!intensity), step::FLAG_SET);
        }
        _ => d.finish(None),
    }
}

/// Mode `entry` set; the VGA's equipment word set to the monochrome or
/// colour mode it is.
fn set_mode(d: &mut Driver<'_, '_>, entry: Entry, hardware: u8) {
    if hardware & hardware::KIND == hardware::VGA {
        let bits = if matches!(entry.mode, 7 | 15) {
            0x30
        } else {
            0x20
        };
        let equipment = d.g.bda16(bda::EQUIPMENT);
        d.g.set_bda16(bda::EQUIPMENT, equipment & !0x30 | bits);
    }
    bios(d, u16::from(entry.mode), 0, step::MODE_SET);
}

/// The enhanced colour display's cursor emulation turned off for more than
/// 25 lines of the 8×8 font, which the emulation would misplace, and back
/// on for 25.
fn emulate_cursor(d: &mut Driver<'_, '_>, hardware: u8, rows: u16) {
    if hardware & hardware::KIND != hardware::EGA_ENHANCED {
        return;
    }
    let info = d.g.bda8(bda::EGA_INFO);
    d.g.set_bda8(
        bda::EGA_INFO,
        if rows == ROWS {
            info & !0x01
        } else {
            info | 0x01
        },
    );
}

/// INIT's first look: which adapters' memory answers, a byte written there
/// read back and put back as it was.
pub fn answering(d: &mut Driver<'_, '_>) -> u8 {
    let mut found = 0;
    for (segment, bit) in [(0xB000, hardware::MONO), (0xB800, hardware::COLOUR)] {
        let kept = d.g.read8(segment, 0);
        d.g.write8(segment, 0, 0x55);
        if d.g.read8(segment, 0) == 0x55 {
            found |= bit;
        }
        d.g.write8(segment, 0, kept);
    }
    found
}

/// The VGA BIOS's state read at INIT: the VGA, the scan-line setting in
/// force and those it offers in text modes. `None` when the call is not the
/// VGA's.
pub fn vga_found(d: &mut Driver<'_, '_>) -> Option<u8> {
    if d.g.r8(Reg8::Al) != 0x1B {
        return None;
    }
    let state = at::VIDEO_STATE;
    if d.own8(state + 0x2D) & 0x01 == 0 {
        return Some(0);
    }
    let in_force = d.own8(state + 0x2A);
    d.set_own8(at::MAX_SCANS, in_force);
    let table = Far::new(d.own16(state + 2), d.own16(state));
    let offered = d.g.read8(table.segment, table.offset.wrapping_add(7));
    d.set_own8(at::SCANS, offered);
    Some(hardware::VGA)
}

/// The EGA BIOS's answer at INIT: its monitor, by the display and the
/// switches.
pub fn ega_found(d: &mut Driver<'_, '_>) -> u8 {
    let [bl, bh] = d.g.r16(Reg16::Bx).to_le_bytes();
    if bl == 0x10 {
        return 0;
    }
    if bh == 1 {
        return hardware::EGA_MONO;
    }
    match d.g.r8(Reg8::Cl) & 0x0F {
        3 | 9 => hardware::EGA_ENHANCED,
        _ => hardware::EGA_COLOUR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn later_tables_replace_earlier_modes() {
        let vga = modes(hardware::COLOUR | hardware::VGA);
        let mode3 = vga.iter().find(|entry| entry.mode == 3).copied();
        assert_eq!(mode3.map(|entry| entry.rows), Some(ANY));
        assert_eq!(vga.iter().filter(|entry| entry.mode == 3).count(), 1);
        assert_eq!(
            vga.first().map(|entry| entry.mode),
            Some(1),
            "the colour table's order is kept"
        );
    }

    #[test]
    fn each_adapter_shows_its_own_modes() {
        let rows = |hardware: u8, mode: u8| {
            modes(hardware)
                .iter()
                .find(|entry| entry.mode == mode)
                .map(|entry| entry.rows)
        };
        assert_eq!(rows(hardware::COLOUR, 3), Some(25));
        assert_eq!(rows(hardware::COLOUR, 7), None);
        assert_eq!(rows(hardware::MONO, 7), Some(25));
        assert_eq!(rows(hardware::EGA_ENHANCED, 3), Some(43));
        assert_eq!(rows(hardware::EGA_MONO, 7), Some(43));
        assert_eq!(rows(hardware::EGA_COLOUR, 3), Some(25));
    }

    #[test]
    fn scan_lines_follow_the_rows_offered() {
        assert_eq!(scans_for(25, 0x07), Some(0));
        assert_eq!(scans_for(43, 0x07), Some(1));
        assert_eq!(scans_for(50, 0x07), Some(2));
        assert_eq!(scans_for(50, 0x03), None);
        assert_eq!(scans_for(30, 0x07), None);
        assert_eq!(scans_for(u16::MAX, 0x07), None);
    }
}
