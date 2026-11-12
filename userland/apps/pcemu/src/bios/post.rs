//! The power-on self test: memory, the interrupt vectors, the chips, the
//! data area and the display; then the option ROMs, and the bootstrap.

use super::guest::Guest;
use super::layout::{self, diskette, entry, table, SEGMENT};
use super::{bda, clock, ems, video};
use crate::asm::Reg16;
use crate::chips::ppi::port_b;
use crate::cpu::{Family, Seg};
use crate::ports::{game, parallel, serial};

/// The segments the option ROM scan looks at, 2 KiB apart.
const ROM_SCAN: core::ops::Range<u16> = layout::ADAPTER_ROMS..0xF400;
const ROM_STEP: u16 = 0x80;
/// The most conventional memory there can be, in KiB.
const MAX_KIB: u16 = 640;

/// The POST's first part, before the option ROMs.
pub fn begin(g: &mut Guest<'_>) {
    let warm = g.bda16(bda::RESET_FLAG) == bda::WARM;
    g.regs_mut().set_sreg(Seg::Ss, 0x0030);
    g.set_r16(Reg16::Sp, 0x0100);
    if g.machine().cpu().model().family() == Family::Intel80186 {
        // The ROM at full speed, and the 8259A on INT0 in cascade mode at the
        // lowest priority, so handlers that end interrupts only at the 8259A
        // keep working.
        g.output_word(0xFFA0, 0xF03C);
        g.output_word(0xFF38, 0x0077);
    }
    let kib = memory(g, warm);
    vectors(g);
    chips(g);
    ems::install(g);
    let switches = switches(g);
    data_area(g, kib, switches);
    let mode = match (switches >> 4) & 3 {
        1 => Some(1),
        2 => Some(3),
        3 => Some(7),
        // The display's own ROM sets its mode.
        _ => None,
    };
    if let Some(mode) = mode {
        video::set_mode(g, mode);
    }
    g.set_bda16(bda::OPTION_ROM, 0);
    g.set_bda16(bda::OPTION_ROM + 2, ROM_SCAN.start);
}

/// Sizes conventional memory a kilobyte at a time, clearing it on a cold
/// start.
fn memory(g: &mut Guest<'_>, warm: bool) -> u16 {
    let mut kib = 0;
    while kib < MAX_KIB {
        let segment = kib * 64;
        let kept = g.read8(segment, 0);
        let holds = [0x55, 0xAA].into_iter().all(|pattern| {
            g.write8(segment, 0, pattern);
            g.read8(segment, 0) == pattern
        });
        if !holds {
            break;
        }
        if warm {
            g.write8(segment, 0, kept);
        } else {
            for offset in 0..1024 {
                g.write8(segment, offset, 0);
            }
        }
        kib += 1;
    }
    kib
}

/// The interrupt vectors the firmware answers.
fn vectors(g: &mut Guest<'_>) {
    let mut set = |n: u16, segment: u16, offset: u16| {
        g.write16(0, n * 4, offset);
        g.write16(0, n * 4 + 2, segment);
    };
    for n in 0..0x20 {
        set(n, SEGMENT, entry::IRET);
    }
    for n in 0x08..0x10 {
        set(n, SEGMENT, entry::EOI);
    }
    for (n, offset) in [
        (0x02, entry::NMI),
        (0x05, entry::PRINT_SCREEN),
        (0x08, entry::TICK),
        (0x09, entry::KEYBOARD_IRQ),
        (0x0E, entry::FLOPPY_IRQ),
        (0x10, entry::VIDEO),
        (0x11, entry::EQUIPMENT),
        (0x12, entry::MEMORY),
        (0x13, entry::DISKETTE),
        (0x14, entry::SERIAL),
        (0x15, entry::SYSTEM),
        (0x16, entry::KEYBOARD),
        (0x17, entry::PRINTER),
        (0x19, entry::BOOT),
        (0x1A, entry::CLOCK),
        (0x1D, table::VIDEO),
        (u16::from(diskette::VECTOR), table::DISKETTE),
        (0x1F, table::FONT_HIGH),
    ] {
        set(n, SEGMENT, offset);
    }
    set(0x18, SEGMENT + (entry::NO_BOOT >> 4), entry::NO_BOOT & 0x0F);
}

/// The planar chips as the machine runs them.
fn chips(g: &mut Guest<'_>) {
    for (port, value) in [
        // The 8259A edge-triggered from vector 08h, everything masked until
        // the POST is done.
        (0x20, 0x13),
        (0x21, 0x08),
        (0x21, 0x09),
        (0x21, 0xFF),
        // The 8253: the day's tick, refresh every 15 µs, the speaker's 896 Hz.
        (0x43, 0x36),
        (0x40, 0x00),
        (0x40, 0x00),
        (0x43, 0x54),
        (0x41, 18),
        (0x43, 0xB6),
        (0x42, 0x33),
        (0x42, 0x05),
        // The 8237 refreshing on channel 0; the others verify until asked.
        (0x0D, 0x00),
        (0x0B, 0x58),
        (0x00, 0x00),
        (0x00, 0x00),
        (0x01, 0xFF),
        (0x01, 0xFF),
        (0x0B, 0x41),
        (0x0B, 0x42),
        (0x0B, 0x43),
        (0x08, 0x00),
        (0x0A, 0x00),
        // The 8255 with A and C in, the keyboard cleared and its clock
        // running.
        (0x63, 0x99),
        (0x61, port_b::KEYBOARD_CLOCK | port_b::KEYBOARD_CLEAR),
        (0x61, port_b::KEYBOARD_CLOCK),
        // NMI off until the POST is done.
        (0xA0, 0x00),
        // The floppy controller out of reset, its motors off.
        (0x3F2, 0x08),
        (0x3F2, 0x0C),
    ] {
        g.output(port, value);
    }
}

/// The configuration switch block SW1, read as the board wires it: on the
/// PC through port A, on the XT a nibble at a time through port C.
fn switches(g: &mut Guest<'_>) -> u8 {
    let pc = g.read8(SEGMENT, table::MODEL) == 0xFF;
    if pc {
        g.output(0x61, port_b::KEYBOARD_CLOCK | port_b::KEYBOARD_CLEAR);
        let sw1 = g.input(0x60);
        g.output(0x61, port_b::KEYBOARD_CLOCK);
        sw1
    } else {
        g.output(0x61, port_b::KEYBOARD_CLOCK | port_b::XT_SW1_HIGH);
        let high = g.input(0x62) & 0x0F;
        g.output(0x61, port_b::KEYBOARD_CLOCK);
        let low = g.input(0x62) & 0x0F;
        // SW1-1 on the XT loops the POST; its drives always include one.
        (high << 4) | low | 0x01
    }
}

/// Lists in the `slots` words of the data area at `table` each of `bases`
/// where `present` finds an adapter, in order; returns how many it found.
fn list_ports(
    g: &mut Guest<'_>,
    table: u16,
    slots: u16,
    bases: &[u16],
    mut present: impl FnMut(&mut Guest<'_>, u16) -> bool,
) -> u16 {
    let mut found = 0u16;
    for &base in bases {
        if present(g, base) {
            g.set_bda16(table + found * 2, base);
            found += 1;
        }
    }
    for empty in found..slots {
        g.set_bda16(table + empty * 2, 0);
    }
    found
}

/// Finds the ports, the serial adapters by their interrupt identification's
/// high bits, which read zero; the parallel by whether their data latch
/// reads back; the game port by its idle one-shots reading low. Returns
/// their bits of the equipment word.
fn ports(g: &mut Guest<'_>) -> u16 {
    let serial = list_ports(g, bda::COM_PORTS, 4, &serial::BASES, |g, base| {
        g.input(base + serial::reg::IIR) & 0xF8 == 0
    });
    let printers = list_ports(g, bda::LPT_PORTS, 3, &parallel::BASES, |g, base| {
        g.output(base + parallel::reg::DATA, 0xAA);
        g.input(base + parallel::reg::DATA) == 0xAA
    });
    let game = g.input(game::PORT) & game::AXES == 0;
    (serial << 9) | (u16::from(game) << 12) | (printers << 14)
}

fn data_area(g: &mut Guest<'_>, kib: u16, sw1: u8) {
    let ports = ports(g);
    g.set_bda16(bda::EQUIPMENT, u16::from(sw1) | ports);
    g.set_bda16(bda::MEMORY_KIB, kib);
    for offset in [bda::KEY_HEAD, bda::KEY_TAIL, bda::KEY_START] {
        g.set_bda16(offset, 0x001E);
    }
    g.set_bda16(bda::KEY_END, 0x003E);
    for printer in 0..3 {
        g.set_bda8(bda::LPT_TIMEOUT + printer, 0x14);
    }
    for port in 0..4 {
        g.set_bda8(bda::COM_TIMEOUT + port, 0x01);
    }
    for offset in [
        bda::SHIFT_FLAGS,
        bda::SHIFT_FLAGS_2,
        bda::ALT_KEYPAD,
        bda::SEEK_STATUS,
        bda::MOTOR_STATUS,
        bda::MOTOR_COUNT,
        bda::DISKETTE_STATUS,
        bda::MIDNIGHT,
        bda::BREAK,
        bda::FIXED_COUNT,
        bda::DATA_RATE,
    ] {
        g.set_bda8(offset, 0);
    }
    for n in 0..4 {
        g.set_bda8(bda::MEDIA_STATE + n, 0);
    }
    g.set_bda16(bda::TICKS, 0);
    g.set_bda16(bda::TICKS + 2, 0);
    clock::time_of_day(g);
}

/// Finds the next option ROM, which the stub then calls at its offset 3:
/// the signature `55h AAh`, its length in 512-byte blocks, and every byte
/// summing to zero.
pub fn next_option_rom(g: &mut Guest<'_>) {
    let mut segment = g.bda16(bda::OPTION_ROM + 2);
    while ROM_SCAN.contains(&segment) {
        if g.read16(segment, 0) == layout::OPTION_ROM_SIGNATURE {
            let blocks = u16::from(g.read8(segment, 2));
            let sum = (0..blocks * 32).fold(0u8, |sum, paragraph| {
                (0..16).fold(sum, |sum, offset| {
                    sum.wrapping_add(g.read8(segment + paragraph, offset))
                })
            });
            if blocks > 0 && sum == 0 {
                let next =
                    segment.saturating_add((blocks * 32).max(ROM_STEP).next_multiple_of(ROM_STEP));
                g.set_bda16(bda::OPTION_ROM, 3);
                g.set_bda16(bda::OPTION_ROM + 2, next);
                g.regs_mut().set_sreg(Seg::Es, segment);
                g.done(false);
                return;
            }
        }
        segment += ROM_STEP;
    }
    g.set_bda16(bda::OPTION_ROM + 2, segment);
    g.done(true);
}

/// The POST's last part: the interrupts the BIOS serves unmasked, NMI on,
/// and the firmware's banner.
pub fn finish(g: &mut Guest<'_>) {
    let mask = g.input(0x21) & !0x43;
    g.output(0x21, mask);
    g.output(0xA0, 0x80);
    let kib = g.bda16(bda::MEMORY_KIB);
    video::write_text(g, b"TAIRiX PC BIOS    ");
    video::write_number(g, kib);
    video::write_text(g, b" KB\r\n\r\n");
    g.set_bda16(bda::RESET_FLAG, 0);
}
