//! The screen modes' hardware settings and tables software reads, the
//! palette, sound flushing, and OS calls meeting the interrupt routines that
//! make OS calls of their own.

use alloc::vec::Vec;

use super::{
    boot, boot_rom, claim, finish, program, record, recorded, run, run_rom, BUDGET, INTERRUPTS_OFF,
};
use crate::machine::bbc::latch;
use crate::machine::bbc::sheila;
use crate::machine::bbc::via::reg;
use crate::rom::asm::{abs, imm, zp, Asm};
use crate::rom::mos::high::DEFAULTS_DESCRIPTOR;
use crate::rom::mos::modes::ROWS_640;
use crate::rom::mos::reset::VECTOR_DEFAULTS_AT;
use crate::rom::mos::{model_b, ws, VECTOR_DEFAULTS};
use crate::rom::os::entry::{OSBYTE, OSWORD, OSWRCH};
use crate::rom::os::{event, vector};
use crate::scenario::type_text;

/// The latch writes setting the screen size, by latch bit number and level.
fn screen_size_writes(machine: &mut crate::machine::bbc::Machine) -> Vec<(u8, bool)> {
    let bits = [
        latch::SCREEN_SIZE.trailing_zeros(),
        latch::SCREEN_SIZE.ilog2(),
    ];
    machine
        .take_io_trace()
        .filter(|access| access.write && access.addr == sheila::SYSTEM_VIA + u16::from(reg::ORB))
        .map(|access| (access.value & 7, access.value & 8 != 0))
        .filter(|&(bit, _)| bits.contains(&u32::from(bit)))
        .collect()
}

/// The regression: modes 0–2 and 4–5 set the latch's screen size as each
/// other's, and mode 7 both bits, where MOS 1.20 is seen to set C0 low and C1
/// high for 20K, both high for 10K, and in mode 7 C0 low twice.
#[test]
fn each_mode_sets_the_screen_size_as_mos_1_20_does() {
    const C0: u8 = 4;
    const C1: u8 = 5;
    let expected: [[(u8, bool); 2]; 8] = [
        [(C0, false), (C1, true)],
        [(C0, false), (C1, true)],
        [(C0, false), (C1, true)],
        [(C0, false), (C1, false)],
        [(C0, true), (C1, true)],
        [(C0, true), (C1, true)],
        [(C0, true), (C1, false)],
        [(C0, false), (C0, false)],
    ];
    for (mode, expected) in (0u8..).zip(expected) {
        let mut machine = boot(&[22, mode], &[]);
        machine.monitor_io(true);
        finish(&mut machine, BUDGET);
        let writes = screen_size_writes(&mut machine);
        assert_eq!(writes[writes.len() - 2..], expected, "mode {mode}");
    }
}

/// The regression: the 640-byte row table lay wherever it was built, low
/// byte first, where software reads MOS 1.20's at &C375, high byte first;
/// &E0 names it in modes 0 to 3.
#[test]
fn the_640_byte_row_table_lies_where_mos_1_20_s_does() {
    let image = model_b().expect("the MOS assembles");
    for row in 0u16..32 {
        let at = usize::from(ROWS_640 - 0xC000 + 2 * row);
        let [low, high] = (row * 640).to_le_bytes();
        assert_eq!(image.bytes[at..at + 2], [high, low], "row {row}");
    }
    for mode in 0..4 {
        let machine = run(&[22, mode], &[]);
        let pointer = u16::from_le_bytes([machine.peek(0xE0), machine.peek(0xE1)]);
        assert_eq!(pointer, ROWS_640, "mode {mode}");
    }
}

/// Each mode's &E0 table holds its rows' offsets high byte first.
#[test]
fn every_mode_s_row_table_is_high_byte_first() {
    let image = model_b().expect("the MOS assembles");
    for (mode, row_bytes, rows) in [(4u8, 320u16, 32u16), (6, 320, 25), (7, 40, 25)] {
        let machine = run(&[22, mode], &[]);
        let table = u16::from_le_bytes([machine.peek(0xE0), machine.peek(0xE1)]);
        for row in 0..rows {
            let at = usize::from(table - 0xC000 + 2 * row);
            let entry = [image.bytes[at], image.bytes[at + 1]];
            assert_eq!(
                u16::from_be_bytes(entry),
                row * row_bytes,
                "mode {mode} row {row}"
            );
        }
    }
}

/// The 6845's registers written, as register and value, the cursor's address
/// apart.
fn crtc_writes(machine: &mut crate::machine::bbc::Machine) -> Vec<(u8, u8)> {
    const CURSOR: [u8; 2] = [14, 15];
    let mut register = 0;
    machine
        .take_io_trace()
        .filter(|access| access.write)
        .filter_map(|access| match access.addr {
            sheila::CRTC_ADDRESS => {
                register = access.value;
                None
            }
            sheila::CRTC_DATA => Some((register, access.value)),
            _ => None,
        })
        .filter(|(register, _)| !CURSOR.contains(register))
        .collect()
}

/// The 6845 writes from boot to the end of a program setting *TV to `shift`
/// and `interlace`, then writing `vdu`.
fn crtc_writes_after_tv(shift: u8, interlace: u8, vdu: &[u8]) -> Vec<(u8, u8)> {
    let vdu = vdu.to_vec();
    let rom = program(None, move |a| {
        a.lda(imm(0x90));
        a.ldx(imm(shift));
        a.ldy(imm(interlace));
        a.jsr(OSBYTE);
        for byte in vdu {
            a.lda(imm(byte));
            a.jsr(OSWRCH);
        }
    });
    let mut machine = boot_rom(&rom);
    machine.monitor_io(true);
    finish(&mut machine, BUDGET);
    crtc_writes(&mut machine)
}

/// The regression: VDU 23,0 wrote R7 its value as given, where MOS 1.20 is
/// seen to move it by *TV's shift and one more, as MODE moves its own; R7's
/// alias R39 is written as given.
#[test]
fn vdu_23_0_moves_r7_by_the_tv_shift_and_one_more() {
    for (shift, value, written) in [
        (0u8, 27u8, 28u8),
        (1, 27, 29),
        (0xFF, 27, 27),
        (0xFE, 27, 26),
        (0, 0xFF, 0),
    ] {
        let vdu = [
            23, 0, 7, value, 0, 0, 0, 0, 0, 0, 23, 0, 39, value, 0, 0, 0, 0, 0, 0,
        ];
        let writes = crtc_writes_after_tv(shift, 0, &vdu);
        assert_eq!(
            writes[writes.len() - 2..],
            [(7, written), (39, value)],
            "*TV {shift}, value {value}"
        );
    }
}

/// The regression: MODE wrote the 6845's registers upwards, with the screen's
/// start before them and again after, where MOS 1.20 is seen to write them
/// from R11 down, then the start once.
#[test]
fn mode_writes_the_6845_downwards_then_its_start_once() {
    let writes = crtc_writes_after_tv(3, 0, &[22, 1]);
    let expected = [
        (11, 8),
        (10, 103),
        (9, 7),
        (8, 1),
        (7, 38),
        (6, 32),
        (5, 0),
        (4, 38),
        (3, 40),
        (2, 98),
        (1, 80),
        (0, 127),
        (12, 6),
        (13, 0),
    ];
    assert_eq!(writes[writes.len() - expected.len()..], expected);
}

/// The regression: MODE 7 took *TV's interlace off as the other modes do,
/// where MOS 1.20 is seen to keep the interlaced video teletext needs.
#[test]
fn mode_7_keeps_its_interlaced_video_whatever_tv_says() {
    for (mode, r8) in [(7u8, 0x93u8), (4, 0x00)] {
        let writes = crtc_writes_after_tv(0, 1, &[22, mode]);
        let last = writes.iter().rev().find(|&&(register, _)| register == 8);
        assert_eq!(last, Some(&(8, r8)), "mode {mode}");
    }
}

/// The regression: the vectors' defaults lay wherever they were built, where
/// software, Jet-Pac among it, copies MOS 1.20's from &D940 to page 2 itself;
/// the descriptor at &FFB6 names the same table.
#[test]
fn software_restoring_the_vectors_from_d940_keeps_a_working_machine() {
    let image = model_b().expect("the MOS assembles");
    let table = usize::from(VECTOR_DEFAULTS_AT - 0xC000);
    for (index, &(_, address)) in VECTOR_DEFAULTS.iter().enumerate() {
        let at = table + 2 * index;
        assert_eq!(
            image.bytes[at..at + 2],
            address.to_le_bytes(),
            "vector {index}"
        );
    }
    let descriptor = usize::from(DEFAULTS_DESCRIPTOR - 0xC000);
    assert_eq!(
        image.bytes[descriptor + 1..descriptor + 3],
        VECTOR_DEFAULTS_AT.to_le_bytes()
    );
    let last = u8::try_from(2 * VECTOR_DEFAULTS.len() - 1).expect("a page of vectors");
    let rom = program(None, move |a| {
        let copy = a.local();
        a.sei();
        a.ldx(imm(last));
        a.bind(copy);
        a.lda(abs(VECTOR_DEFAULTS_AT).x());
        a.sta(abs(vector::USER).x());
        a.dex();
        a.bpl(copy);
        a.cli();
        // INKEY(10) waits on the tick through IRQ1V.
        a.lda(imm(0x81));
        a.ldx(imm(10));
        a.ldy(imm(0));
        a.jsr(OSBYTE);
        a.lda(imm(b'A'));
        a.jsr(OSWRCH);
    });
    run_rom(&rom);
}

/// The palette register writes the VDU bytes `vdu` make, and &249 after.
fn palette_writes(vdu: &[u8]) -> (Vec<u8>, u8) {
    let mut machine = boot(vdu, &[]);
    machine.monitor_io(true);
    finish(&mut machine, BUDGET);
    let writes = machine
        .take_io_trace()
        .filter(|access| access.write && access.addr == sheila::VIDEO_PALETTE)
        .map(|access| access.value)
        .collect();
    (writes, machine.peek(ws::var::ULA_PALETTE))
}

/// The regression: VDU 19 rewrote all sixteen ULA entries, where MOS 1.20
/// writes only those of the colour it changes, lowest first, leaving any
/// software wrote itself; &249 keeps the last written.
#[test]
fn vdu_19_writes_only_the_entries_of_its_colour() {
    for (mode, entries) in [(2u8, &[0x15u8][..]), (1, &[0x25, 0x35, 0x65, 0x75])] {
        let (moded, _) = palette_writes(&[22, mode]);
        let (written, copy) = palette_writes(&[22, mode, 19, 1, 2, 0, 0, 0]);
        assert_eq!(
            written[..moded.len()],
            moded[..],
            "mode {mode}: MODE's writes first"
        );
        assert_eq!(
            written[moded.len()..],
            *entries,
            "mode {mode}: colour 1's entries"
        );
        assert_eq!(copy, entries[entries.len() - 1], "mode {mode}: the copy");
    }
}

/// The regression: OSBYTE &9B wrote X to the ULA as given, where MOS 1.20
/// writes the physical colour inverted, as the ULA takes it, and keeps that.
#[test]
fn osbyte_155_writes_the_physical_colour_inverted() {
    let rom = program(None, |a| {
        a.lda(imm(0x9B));
        a.ldx(imm(0x85));
        a.jsr(OSBYTE);
    });
    let mut machine = boot_rom(&rom);
    machine.monitor_io(true);
    finish(&mut machine, BUDGET);
    let last = machine
        .take_io_trace()
        .filter(|access| access.write && access.addr == sheila::VIDEO_PALETTE)
        .last()
        .map(|access| access.value);
    assert_eq!(last, Some(0x82));
    assert_eq!(machine.peek(ws::var::ULA_PALETTE), 0x82);
}

/// The audio's level over the next `centiseconds`.
fn level(machine: &mut crate::machine::bbc::Machine, centiseconds: u64) -> f32 {
    machine.take_audio().for_each(drop);
    crate::scenario::wait(machine, centiseconds);
    let samples: Vec<f32> = machine.take_audio().collect();
    let count = samples.iter().map(|_| 1.0).sum::<f32>().max(1.0);
    let mean = samples.iter().sum::<f32>() / count;
    (samples.iter().map(|s| (s - mean) * (s - mean)).sum::<f32>() / count).sqrt()
}

/// The regression: flushing a channel's queue left the note it was playing
/// sounding for ever, its channel idle but its volume still on the chip,
/// where MOS 1.20 silences it at once.
#[test]
fn flushing_a_queue_silences_the_note_playing() {
    const BLOCK: u16 = 0x0074;
    const QUEUED: u16 = 0x0090;
    let rom = program(None, |a| {
        // SOUND 1,-15,100,200; a while; then OSBYTE 21 flushes channel 1.
        for (offset, byte) in (0..).zip([1, 0, 0xF1, 0xFF, 100, 0, 200, 0]) {
            a.lda(imm(byte));
            a.sta(zp(BLOCK + offset));
        }
        a.lda(imm(7));
        a.ldx(imm(BLOCK.to_le_bytes()[0]));
        a.ldy(imm(0));
        a.jsr(OSWORD);
        a.lda(imm(1));
        a.sta(zp(QUEUED));
        a.ldx(imm(0));
        a.ldy(imm(0));
        a.lda(imm(4));
        let delay = a.here_local();
        a.dex();
        a.bne(delay);
        a.dey();
        a.bne(delay);
        a.sec();
        a.sbc(imm(1));
        a.bne(delay);
        a.lda(imm(21));
        a.ldx(imm(5));
        a.jsr(OSBYTE);
    });
    let mut machine = boot_rom(&rom);
    machine.set_sound_output(true);
    while machine.peek(QUEUED) != 1 {
        crate::scenario::wait(&mut machine, 1);
    }
    assert!(level(&mut machine, 20) > 0.05, "the note sounds");
    finish(&mut machine, BUDGET);
    assert!(level(&mut machine, 20) < 0.001, "the flush silenced it");
}

/// The regression: the bell counted its queue's room with interrupts on, so
/// an interrupt routine's SOUND could take the room before the bell's three
/// bytes went in; the count and the filling are one, with interrupts off.
#[test]
fn the_bell_counts_and_fills_its_queue_with_interrupts_off() {
    const OLD: u16 = 0x0076;
    const SEEN: u16 = 0x0074;
    let rom = program(None, |a| {
        claim(a, vector::CNP, "count", OLD);
        a.lda(imm(7));
        a.jsr(OSWRCH);
        a.jmp("rung");
        a.here("count");
        a.php();
        a.pha();
        a.php();
        a.pla();
        a.sta(zp(SEEN));
        a.pla();
        a.plp();
        a.jmp(abs(OLD).ind());
        a.here("rung");
    });
    let machine = run_rom(&rom);
    assert_ne!(machine.peek(SEEN) & INTERRUPTS_OFF, 0);
}

/// An OSBYTE call gives back the interrupt state its caller had, whichever.
#[test]
fn osbyte_returns_with_its_caller_s_interrupt_state() {
    const STATES: u16 = 0x0074;
    let rom = program(None, |a| {
        for (at, masked) in [(STATES, false), (STATES + 4, true)] {
            if masked {
                a.sei();
            } else {
                a.cli();
            }
            a.lda(imm(0x86));
            a.jsr(OSBYTE);
            record(a, at);
        }
        a.cli();
    });
    let machine = run_rom(&rom);
    assert_eq!(
        recorded(&machine, STATES)[3] & INTERRUPTS_OFF,
        0,
        "called with I clear"
    );
    assert_ne!(
        recorded(&machine, STATES + 4)[3] & INTERRUPTS_OFF,
        0,
        "called with I set"
    );
}

/// Assemble an interrupt routine for the VSYNC event at `event_routine`
/// that makes OSBYTE &86 on every frame, overwriting the call registers.
fn interfering_event(a: &mut Asm) {
    a.here("event_routine");
    a.cmp(imm(event::VSYNC));
    let other = a.local();
    a.bne(other);
    a.pha();
    a.txa();
    a.pha();
    a.tya();
    a.pha();
    a.lda(imm(0x86));
    a.ldx(imm(0xEE));
    a.ldy(imm(0xEE));
    a.jsr(OSBYTE);
    a.pla();
    a.tay();
    a.pla();
    a.tax();
    a.pla();
    a.bind(other);
    a.rts();
}

/// The regression: an OS call waiting for an interrupt let interrupt
/// routines in with nothing kept, so their own calls overwrote the call
/// registers it returns: OSWORD 0, waiting for each key, gave back A and X
/// as an interrupt routine's OSBYTE left them.
#[test]
fn osword_0_keeps_its_registers_while_interrupt_routines_make_calls() {
    const OLD: u16 = 0x0076;
    const BLOCK: u16 = 0x0078;
    const LINE: u16 = 0x0900;
    const RESULT: u16 = 0x0080;
    let [line_low, line_high] = LINE.to_le_bytes();
    let rom = program(None, |a| {
        claim(a, vector::EVENT, "event_routine", OLD);
        a.lda(imm(14));
        a.ldx(imm(event::VSYNC));
        a.jsr(OSBYTE);
        for (offset, byte) in (0..).zip([line_low, line_high, 20, b' ', b'~']) {
            a.lda(imm(byte));
            a.sta(zp(BLOCK + offset));
        }
        a.lda(imm(0));
        a.ldx(imm(BLOCK.to_le_bytes()[0]));
        a.ldy(imm(0));
        a.jsr(OSWORD);
        record(a, RESULT);
        a.lda(imm(13));
        a.ldx(imm(event::VSYNC));
        a.jsr(OSBYTE);
        a.jmp("read");
        interfering_event(a);
        a.here("read");
    });
    let mut machine = boot_rom(&rom);
    crate::scenario::wait(&mut machine, 150);
    type_text(&mut machine, "HELLO\r").expect("typed");
    finish(&mut machine, BUDGET);
    let [call, block, length, flags] = recorded(&machine, RESULT);
    assert_eq!(
        (call, block),
        (0, BLOCK.to_le_bytes()[0]),
        "A and X as given"
    );
    assert_eq!(length, 5, "Y, the line's length");
    assert_eq!(flags & super::CARRY, 0, "no Escape");
    let line: Vec<u8> = (0..6).map(|i| machine.peek(LINE + i)).collect();
    assert_eq!(line, b"HELLO\r");
}
