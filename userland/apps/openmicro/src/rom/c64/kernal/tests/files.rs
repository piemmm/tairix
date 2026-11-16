//! The logical file layer, with no device on the serial bus: each call's
//! error and STATUS as the original's gives them, the table's limits,
//! RS-232's buffers moving the top of memory, and its frames on TXD.

use alloc::vec::Vec;

use super::{entry, machine, program, run, settled, DONE, DONE_MARK};
use crate::machine::c64::{Machine, Standard};
use crate::rom::asm::{abs, hi, imm, lo, Asm};
use crate::rom::c64::kernal::rs232::TXD;
use crate::rom::c64::kernal::ws::{page, zp};
use crate::rom::c64::testing::run_for;

/// Where a test keeps each call's A, X, Y, flags and STATUS, five bytes a
/// call.
const RESULTS: u16 = 0xC010;

fn record(a: &mut Asm, call: u16) {
    let at = RESULTS + 5 * call;
    a.php();
    a.sta(abs(at));
    a.stx(abs(at + 1));
    a.sty(abs(at + 2));
    a.pla();
    a.sta(abs(at + 3));
    a.lda(abs(zp::STATUS));
    a.sta(abs(at + 4));
}

/// Call `call`'s A, whether it set C, and its STATUS.
fn outcome(machine: &Machine, call: u16) -> (u8, bool, u8) {
    let at = RESULTS + 5 * call;
    (
        machine.ram(at),
        machine.ram(at + 3) & 0x01 != 0,
        machine.ram(at + 4),
    )
}

fn setlfs(a: &mut Asm, file: u8, device: u8, secondary: u8) {
    a.lda(imm(file));
    a.ldx(imm(device));
    a.ldy(imm(secondary));
    a.jsr(entry::SETLFS);
}

fn setnam(a: &mut Asm, name: &'static [u8]) {
    a.lda(imm(crate::rom::asm::byte(name.len())));
    a.ldx(imm(lo("file_name")));
    a.ldy(imm(hi("file_name")));
    a.jsr(entry::SETNAM);
    a.later(move |a| {
        a.here("file_name");
        a.bytes(name);
    });
}

fn ran(body: impl FnOnce(&mut Asm)) -> Machine {
    let mut machine = settled(Standard::Pal, &program(body));
    run(&mut machine, 10);
    machine
}

#[test]
fn opening_a_file_numbered_0_is_refused() {
    let machine = ran(|a| {
        setnam(a, b"");
        setlfs(a, 0, 3, 0);
        a.jsr(entry::OPEN);
        record(a, 0);
    });
    assert_eq!(outcome(&machine, 0), (6, true, 0), "NOT INPUT FILE");
}

#[test]
fn the_table_takes_ten_files_and_each_number_once() {
    let machine = ran(|a| {
        setnam(a, b"");
        for file in 1..=11 {
            setlfs(a, file, 3, 0);
            a.jsr(entry::OPEN);
            record(a, u16::from(file - 1));
        }
        setlfs(a, 1, 3, 0);
        a.jsr(entry::OPEN);
        record(a, 11);
    });
    for call in 0..10 {
        assert!(!outcome(&machine, call).1, "file {} opens", call + 1);
    }
    assert_eq!(outcome(&machine, 10), (1, true, 0), "TOO MANY FILES");
    assert_eq!(outcome(&machine, 11), (2, true, 0), "FILE OPEN");
    assert_eq!(machine.ram(zp::LDTND), 10);
}

#[test]
fn channels_need_files_open_the_right_way() {
    let machine = ran(|a| {
        a.ldx(imm(5));
        a.jsr(entry::CHKIN);
        record(a, 0);
        setnam(a, b"");
        setlfs(a, 1, 0, 0);
        a.jsr(entry::OPEN);
        a.ldx(imm(1));
        a.jsr(entry::CHKOUT);
        record(a, 1);
        a.jsr(entry::CLRCHN);
        a.lda(imm(1));
        a.jsr(entry::CLOSE);
        record(a, 2);
        a.lda(imm(7));
        a.jsr(entry::CLOSE);
        record(a, 3);
    });
    assert_eq!(outcome(&machine, 0), (3, true, 0), "FILE NOT OPEN");
    assert_eq!(
        outcome(&machine, 1),
        (7, true, 0),
        "NOT OUTPUT FILE: the keyboard"
    );
    assert!(!outcome(&machine, 2).1, "closed");
    assert!(!outcome(&machine, 3).1, "a file not open closes quietly");
}

#[test]
fn loads_need_a_device_that_can_and_a_name_for_the_bus() {
    let machine = ran(|a| {
        setnam(a, b"X");
        setlfs(a, 1, 3, 0);
        a.lda(imm(0));
        a.jsr(entry::LOAD);
        record(a, 0);
        a.lda(imm(0));
        a.ldx(imm(0));
        a.ldy(imm(0));
        a.jsr(entry::SETNAM);
        setlfs(a, 1, 8, 0);
        a.lda(imm(0));
        a.jsr(entry::LOAD);
        record(a, 1);
    });
    assert_eq!(outcome(&machine, 0), (9, true, 0), "ILLEGAL DEVICE NUMBER");
    assert_eq!(outcome(&machine, 1), (8, true, 0), "MISSING FILE NAME");
}

#[test]
fn a_device_absent_from_the_bus_is_not_present() {
    let machine = ran(|a| {
        a.lda(imm(0x80));
        a.jsr(entry::SETMSG);
        setnam(a, b"X");
        setlfs(a, 1, 8, 0);
        a.lda(imm(0));
        a.ldx(imm(0x01));
        a.ldy(imm(0x08));
        a.jsr(entry::LOAD);
        record(a, 0);
        setlfs(a, 2, 9, 2);
        a.jsr(entry::OPEN);
        record(a, 1);
    });
    assert_eq!(outcome(&machine, 0), (5, true, 0x80), "DEVICE NOT PRESENT");
    assert_eq!(outcome(&machine, 1), (5, true, 0x80));
    let row: alloc::vec::Vec<u8> = (0..15)
        .map(|column| machine.ram(0x0400 + 40 + column))
        .collect();
    assert_eq!(
        row, *b"\x13\x05\x01\x12\x03\x08\x09\x0E\x07 \x06\x0F\x12 \x18",
        "SEARCHING FOR X"
    );
}

#[test]
fn rs232_buffers_come_off_the_top_of_memory_and_go_back() {
    let machine = ran(|a| {
        setnam(a, &[0x08, 0x00]);
        setlfs(a, 2, 2, 0);
        a.jsr(entry::OPEN);
        record(a, 0);
        a.ldx(imm(2));
        a.jsr(entry::CHKIN);
        a.jsr(entry::GETIN);
        record(a, 1);
        a.jsr(entry::READST);
        record(a, 2);
        a.jsr(entry::CLRCHN);
        a.lda(abs(page::MEMSIZ + 1));
        a.sta(abs(0xC001));
        a.lda(imm(2));
        a.jsr(entry::CLOSE);
        record(a, 3);
    });
    let (moved, carry, _) = outcome(&machine, 0);
    assert_eq!((moved, carry), (0xF0, true), "the top of memory moved");
    assert_eq!(machine.ram(RESULTS + 2), 0x9E, "and is in Y");
    assert_eq!(machine.ram(0xC001), 0x9E, "two pages below BASIC's ROM");
    assert_eq!(outcome(&machine, 1).0, 0, "nothing came in");
    assert_eq!(outcome(&machine, 2).0, 0x08, "RSSTAT: the buffer was empty");
    assert_eq!(outcome(&machine, 3).0, 0xF0, "the top of memory moved back");
    assert_eq!(machine.ram(page::MEMSIZ + 1), 0xA0);
}

#[test]
fn every_rs232_open_and_close_reports_the_top_of_memory() {
    let machine = ran(|a| {
        setnam(a, &[0x06]);
        setlfs(a, 2, 2, 0);
        a.jsr(entry::OPEN);
        setlfs(a, 3, 2, 0);
        a.jsr(entry::OPEN);
        record(a, 0);
        a.lda(imm(3));
        a.jsr(entry::CLOSE);
        record(a, 1);
        a.lda(imm(2));
        a.jsr(entry::CLOSE);
        record(a, 2);
    });
    for (call, top, what) in [
        (0, 0x9E, "a second file's buffers"),
        (1, 0xA0, "given back"),
        (2, 0xA0, "already given back"),
    ] {
        let (code, carry, _) = outcome(&machine, call);
        assert_eq!((code, carry), (0xF0, true), "{what}");
        assert_eq!(
            machine.ram(RESULTS + 5 * call + 1),
            0x00,
            "{what}: the top's low byte in X"
        );
        assert_eq!(
            machine.ram(RESULTS + 5 * call + 2),
            top,
            "{what}: the top's high byte in Y"
        );
    }
}

/// How often TXD is sampled, in cycles: an eightieth of a bit at 300 baud.
const SAMPLE: u64 = 41;

/// TXD from power-on until the program marks it has run to its end: each
/// sample's cycle and level.
fn txd_levels(basic: &[u8]) -> Vec<(u64, bool)> {
    let mut machine = machine(Standard::Pal, basic);
    let end = 8 * Standard::Pal.clock();
    let mut levels = Vec::new();
    while machine.ram(DONE) != DONE_MARK {
        assert!(machine.cycles() < end, "the program finished");
        levels.push((machine.cycles(), machine.cia2_port_a() & TXD != 0));
        run_for(&mut machine, SAMPLE);
    }
    levels
}

/// Each frame in `levels`: when its start bit fell, then `bits` more read
/// at their middles, `bit` cycles apart.
fn frames(levels: &[(u64, bool)], bit: u64, bits: u64) -> Vec<(u64, Vec<bool>)> {
    let level_at = |cycle: u64| {
        let after = levels.partition_point(|&(at, _)| at <= cycle);
        levels.get(after.checked_sub(1)?).map(|&(_, level)| level)
    };
    let mut found = Vec::new();
    let mut previous = true;
    let mut frame_end = 0;
    for &(at, level) in levels {
        if previous && !level && at >= frame_end {
            let read = (1..=bits)
                .map(|index| level_at(at + index * bit + bit / 2).expect("the frame was sampled"));
            found.push((at, read.collect()));
            frame_end = at + bits * bit + bit / 2;
        }
        previous = level;
    }
    found
}

/// `byte`'s low `width` bits, the lowest first, as a frame sends them.
fn word(byte: u8, width: u32) -> impl Iterator<Item = bool> {
    (0..width).map(move |bit| byte >> bit & 1 != 0)
}

#[test]
fn chrout_to_rs232_sends_each_byte_framed_at_the_rate() {
    let bit = Standard::Pal.clock() / 300;
    let formats: [(&'static [u8], u32, bool, usize); 2] =
        [(&[0x06, 0x00], 8, false, 1), (&[0xA6, 0x60], 7, true, 2)];
    for (name, width, even_parity, stops) in formats {
        let levels = txd_levels(&program(|a| {
            setnam(a, name);
            setlfs(a, 2, 2, 0);
            a.jsr(entry::OPEN);
            a.ldx(imm(2));
            a.jsr(entry::CHKOUT);
            for byte in *b"AZ" {
                a.lda(imm(byte));
                a.jsr(entry::CHROUT);
            }
            a.jsr(entry::CLRCHN);
            a.lda(imm(2));
            a.jsr(entry::CLOSE);
        }));
        let expected: Vec<Vec<bool>> = b"AZ"
            .iter()
            .copied()
            .map(|byte| {
                let mut frame: Vec<bool> = word(byte, width).collect();
                if even_parity {
                    frame.push(word(byte, width).filter(|&one| one).count() % 2 == 1);
                }
                frame.extend(core::iter::repeat_n(true, stops));
                frame
            })
            .collect();
        let bits = u64::from(width) + u64::from(even_parity) + u64::try_from(stops).unwrap();
        let sent = frames(&levels, bit, bits);
        assert_eq!(
            sent.iter()
                .map(|(_, frame)| frame.clone())
                .collect::<Vec<_>>(),
            expected,
            "{width} bits"
        );
        // A's first data bit is a one, so its start bit is all it is low for.
        let (fell, _) = sent[0];
        let rose = levels
            .iter()
            .find(|&&(at, level)| at > fell && level)
            .map(|&(at, _)| at)
            .unwrap();
        assert!(
            (rose - fell).abs_diff(bit) <= 2 * SAMPLE,
            "a start bit of {} cycles",
            rose - fell
        );
    }
}

#[test]
fn enabl_holds_the_rs232_interrupts_as_the_original_keeps_them() {
    let machine = ran(|a| {
        let keep = |a: &mut Asm, at: u16| {
            a.lda(abs(page::ENABL));
            a.sta(abs(RESULTS + at));
        };
        setnam(a, &[0x06]);
        setlfs(a, 2, 2, 0);
        a.jsr(entry::OPEN);
        keep(a, 0);
        a.ldx(imm(2));
        a.jsr(entry::CHKIN);
        keep(a, 1);
        a.jsr(entry::CLRCHN);
        keep(a, 2);
        a.ldx(imm(2));
        a.jsr(entry::CHKOUT);
        a.lda(imm(b'A'));
        a.jsr(entry::CHROUT);
        keep(a, 3);
        a.jsr(entry::CLRCHN);
        a.lda(imm(2));
        a.jsr(entry::CLOSE);
        keep(a, 4);
    });
    let kept: Vec<u8> = (0..5).map(|at| machine.ram(RESULTS + at)).collect();
    assert_eq!(
        kept,
        [0x00, 0x90, 0x90, 0x91, 0x00],
        "after OPEN, CHKIN, CLRCHN, CHROUT and CLOSE"
    );
}
