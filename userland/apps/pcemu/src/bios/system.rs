//! The system services: the timer tick, the equipment and memory reports,
//! the XT's INT 15h, the bootstrap, the time of day, print screen and the
//! NMI.

use super::guest::Guest;
use super::layout::BOOTSTRAP;
use super::{bda, clock, video};
use crate::asm::{Reg16, Reg8};
use crate::cpu::{flag, Seg};
use crate::time::{Nanos, NANOS_PER_SECOND};
use tairix_fat::boot::{at as boot_at, SIGNATURE};

/// Ticks in a day: the PIT's 1,193,182 Hz over 65,536, times 86,400.
pub(crate) const TICKS_PER_DAY: u32 = 0x18_00B0;
/// Where the CPU's flags lie above the stack pointer in a direct service.
pub(super) const FLAGS: u16 = 4;

/// The tick count in the data area.
pub(super) fn set_ticks(g: &mut Guest<'_>, ticks: u32) {
    let bytes = ticks.to_le_bytes();
    g.set_bda16(bda::TICKS, u16::from_le_bytes([bytes[0], bytes[1]]));
    g.set_bda16(bda::TICKS + 2, u16::from_le_bytes([bytes[2], bytes[3]]));
}

/// A resumed machine's time of day: its tick count moved on by `away`, as an
/// APM BIOS moves it on at resume, each midnight passed counted in the
/// rollover byte.
pub fn resumed(g: &mut Guest<'_>, away: Nanos) {
    let ticks = u32::from(g.bda16(bda::TICKS + 2)) << 16 | u32::from(g.bda16(bda::TICKS));
    let day = u128::from(TICKS_PER_DAY);
    let gone = u128::from(away) * day / (86_400 * u128::from(NANOS_PER_SECOND));
    let total = u128::from(ticks) + gone;
    set_ticks(g, u32::try_from(total % day).unwrap_or(0));
    let midnights = u8::try_from(total / day).unwrap_or(u8::MAX);
    let counted = g.bda8(bda::MIDNIGHT).saturating_add(midnights);
    g.set_bda8(bda::MIDNIGHT, counted);
}

/// INT 08h: the tick, the day's rollover and the diskette motors' timeout.
/// The rollover byte counts the midnights not yet read, as later BIOSes keep
/// it, so none is lost however long it goes unread.
pub fn tick(g: &mut Guest<'_>) {
    let low = g.bda16(bda::TICKS);
    let high = g.bda16(bda::TICKS + 2);
    // The count is guest memory: one written past a day rolls over too.
    let mut ticks = (u32::from(high) << 16 | u32::from(low)).saturating_add(1);
    if ticks >= TICKS_PER_DAY {
        ticks = 0;
        let counted = g.bda8(bda::MIDNIGHT).saturating_add(1);
        g.set_bda8(bda::MIDNIGHT, counted);
    }
    set_ticks(g, ticks);
    let count = g.bda8(bda::MOTOR_COUNT);
    if count > 0 {
        g.set_bda8(bda::MOTOR_COUNT, count - 1);
        if count == 1 {
            let motors = g.bda8(bda::MOTOR_STATUS);
            g.set_bda8(bda::MOTOR_STATUS, motors & 0xF0);
            g.output(0x3F2, 0x0C);
        }
    }
}

/// INT 11h.
pub fn equipment(g: &mut Guest<'_>) {
    let word = g.bda16(bda::EQUIPMENT);
    g.set_r16(Reg16::Ax, word);
}

/// INT 12h.
pub fn memory(g: &mut Guest<'_>) {
    let kib = g.bda16(bda::MEMORY_KIB);
    g.set_r16(Reg16::Ax, kib);
}

/// INT 15h as an XT answers it: no cassette and no extended services, but
/// the keyboard intercept passes every scan code on.
pub fn system(g: &mut Guest<'_>) {
    if g.r8(Reg8::Ah) != 0x4F {
        g.set_r8(Reg8::Ah, 0x86);
    }
    g.set_returned_flag(FLAGS, flag::CF, true);
}

/// INT 1Ah: the tick count, and the clock card's time and date.
pub fn clock(g: &mut Guest<'_>) {
    match g.r8(Reg8::Ah) {
        0x00 => {
            let (low, high) = (g.bda16(bda::TICKS), g.bda16(bda::TICKS + 2));
            let midnight = g.bda8(bda::MIDNIGHT);
            g.set_bda8(bda::MIDNIGHT, 0);
            g.set_r16(Reg16::Cx, high);
            g.set_r16(Reg16::Dx, low);
            g.set_r8(Reg8::Al, midnight);
        }
        0x01 => {
            let (high, low) = (g.r16(Reg16::Cx), g.r16(Reg16::Dx));
            g.set_bda16(bda::TICKS, low);
            g.set_bda16(bda::TICKS + 2, high);
            g.set_bda8(bda::MIDNIGHT, 0);
        }
        function @ 0x02..=0x07 => clock::service(g, function),
        _ => {}
    }
}

/// INT 18h, with nothing to boot: says so, and the stub waits for a key and
/// tries again.
pub fn no_boot(g: &mut Guest<'_>) {
    video::write_text(g, b"\r\nNo system disk. Insert one and press a key.\r\n");
}

/// The bootstrap's attempts: each drive's tries at reading its first sector
/// to `0000:7C00`, a reset before every one.
const ATTEMPTS: [(u8, u8); 2] = [(0x00, 3), (0x80, 2)];

/// INT 19h: chooses the stub's next INT 13h call, or none is left. The stub
/// keeps the attempt number at the top of the stack.
pub fn boot(g: &mut Guest<'_>) {
    let attempt = g.local(0);
    let fixed = g.bda8(bda::FIXED_COUNT) > 0;
    let mut n = attempt / 2;
    let mut chosen = None;
    for &(drive, tries) in &ATTEMPTS {
        if drive & 0x80 != 0 && !fixed {
            continue;
        }
        if n < tries {
            chosen = Some(drive);
            break;
        }
        n -= tries;
    }
    let Some(drive) = chosen else {
        g.done(true);
        return;
    };
    let read = !attempt.is_multiple_of(2);
    g.set_r16(Reg16::Ax, if read { 0x0201 } else { 0x0000 });
    g.set_r16(Reg16::Bx, BOOTSTRAP);
    g.set_r16(Reg16::Cx, 0x0001);
    g.set_r16(Reg16::Dx, u16::from(drive));
    g.regs_mut().set_sreg(Seg::Es, 0);
    g.set_local(0, attempt + 1);
    g.done(false);
}

/// After the stub's INT 13h: a sector read without error starts it, as long
/// as a fixed disk's carries the boot signature.
pub fn boot_result(g: &mut Guest<'_>) {
    let attempt = g.local(0);
    let was_read = attempt.is_multiple_of(2);
    if !was_read || g.regs().flag(flag::CF) {
        return;
    }
    let drive = g.r8(Reg8::Dl);
    let signature_at =
        u16::try_from(usize::from(BOOTSTRAP) + boot_at::SIGNATURE).unwrap_or(u16::MAX);
    if drive & 0x80 != 0 && g.read16(0, signature_at).to_le_bytes() != SIGNATURE {
        return;
    }
    g.set_r16(Reg16::Dx, u16::from(drive));
    g.jump(0, BOOTSTRAP);
}

/// The print-screen status at `0050:0000`: in progress, or failed.
const PRINTING: u8 = 1;
const PRINT_FAILED: u8 = 0xFF;
/// The print stub's state: the step reached, and the rows it prints.
pub const PRINT_STATE: u8 = 4;
/// The rows the system BIOS prints, as the PC's and XT's did.
pub const PRINT_ROWS: u8 = 25;

/// INT 05h: begins printing `rows` of the screen, one INT 17h a character,
/// unless a print is already under way.
pub fn print_screen(g: &mut Guest<'_>, rows: u8) {
    if g.read8(bda::PRINT_STATUS_SEGMENT, 0) == PRINTING {
        g.done(true);
        return;
    }
    g.write8(bda::PRINT_STATUS_SEGMENT, 0, PRINTING);
    g.set_local(2, rows);
    print_next(g, 0);
}

/// After each INT 17h: the next character, or the print ends.
pub fn print_screen_next(g: &mut Guest<'_>) {
    if g.r8(Reg8::Ah) & 0x29 != 0 {
        g.write8(bda::PRINT_STATUS_SEGMENT, 0, PRINT_FAILED);
        g.done(true);
        return;
    }
    let step = u16::from_le_bytes([g.local(0), g.local(1)]).saturating_add(1);
    print_next(g, step);
}

/// Hands the stub step `step` of the print: a new line, then each row's
/// characters and its own.
fn print_next(g: &mut Guest<'_>, step: u16) {
    // The columns a print loops by are the byte INT 10h's mode query answers.
    let columns = u16::from(g.bda8(bda::COLUMNS));
    let line = columns + 2;
    // A print longer than the step counter holds ends with the counter.
    let total = u16::try_from(2 + u32::from(g.local(2)) * u32::from(line)).unwrap_or(u16::MAX);
    if step >= total {
        g.write8(bda::PRINT_STATUS_SEGMENT, 0, 0);
        g.done(true);
        return;
    }
    let byte = match step {
        0 => b'\r',
        1 => b'\n',
        _ => {
            let (row, column) = ((step - 2) / line, (step - 2) % line);
            match column {
                c if c == columns => b'\r',
                c if c > columns => b'\n',
                c => match video::character_at(
                    g,
                    u8::try_from(row).unwrap_or(0),
                    u8::try_from(c).unwrap_or(0),
                ) {
                    0 => b' ',
                    ch => ch,
                },
            }
        }
    };
    let [low, high] = step.to_le_bytes();
    g.set_local(0, low);
    g.set_local(1, high);
    g.set_r16(Reg16::Ax, u16::from(byte));
    g.set_r16(Reg16::Dx, 0);
    g.done(false);
}

/// INT 02h: a parity or I/O channel check stops the machine with its
/// message; any other NMI belongs to whoever hooked it.
pub fn nmi(g: &mut Guest<'_>) {
    let checks = g.input(0x62) & 0xC0;
    if checks == 0 {
        return;
    }
    let message: &[u8] = if checks & 0x80 != 0 {
        b"\r\nPARITY CHECK 1"
    } else {
        b"\r\nPARITY CHECK 2"
    };
    video::write_text(g, message);
    g.regs_mut().set_flag(flag::IF, false);
    g.jump(super::layout::SEGMENT, super::layout::entry::HALT);
}
