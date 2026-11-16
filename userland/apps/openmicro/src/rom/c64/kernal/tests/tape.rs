//! The datasette: what SAVE records, pulse by pulse, against the original's
//! format as its recordings show it, and LOAD, VERIFY and data files read
//! back from what this KERNAL records.

use alloc::vec::Vec;

use super::{entry, press, program, run, run_for, settled, JIFFY};
use crate::machine::c64::keyboard::Key;
use crate::machine::c64::{Machine, Standard};
use crate::media::tap::Tape;
use crate::rom::asm::{abs, byte, hi, imm, lo, Asm};
use crate::rom::c64::kernal::ws::zp;

/// The program saved: twelve bytes at `$0801`, as the original's test tape
/// holds `10 PRINT"HI"`.
const PROGRAM: [u8; 12] = [
    0x0B, 0x08, 0x0A, 0x00, 0x99, 0x22, 0x48, 0x49, 0x22, 0x00, 0x00, 0x00,
];

fn setnam(a: &mut Asm, name: &'static [u8]) {
    a.lda(imm(byte(name.len())));
    a.ldx(imm(lo("file_name")));
    a.ldy(imm(hi("file_name")));
    a.jsr(entry::SETNAM);
    a.later(move |a| {
        a.here("file_name");
        a.bytes(name);
    });
}

fn setlfs(a: &mut Asm, file: u8, device: u8, secondary: u8) {
    a.lda(imm(file));
    a.ldx(imm(device));
    a.ldy(imm(secondary));
    a.jsr(entry::SETLFS);
}

/// [`PROGRAM`] at `$0801`, saved as "AB".
fn saving() -> Vec<u8> {
    program(|a| {
        for (offset, &value) in (0..).zip(&PROGRAM) {
            a.lda(imm(value));
            a.sta(abs(0x0801 + offset));
        }
        setnam(a, b"AB");
        setlfs(a, 1, 1, 0);
        a.lda(imm(0x01));
        a.sta(abs(0xFB));
        a.lda(imm(0x08));
        a.sta(abs(0xFC));
        a.lda(imm(0xFB));
        a.ldx(imm(0x0D));
        a.ldy(imm(0x08));
        a.jsr(entry::SAVE);
        a.php();
        a.pla();
        a.sta(abs(0xC001));
    })
}

/// What the machine records on a blank tape with RECORD and PLAY down.
fn recorded(basic: &[u8]) -> Tape {
    let mut machine = settled(Standard::Pal, basic);
    machine.insert_tape(Tape::blank());
    machine.press_play(true);
    run(&mut machine, 60);
    assert_eq!(machine.ram(0xC001) & 0x01, 0, "the save succeeded");
    machine.stop_tape();
    machine.datasette().tape().cloned().expect("the tape")
}

/// A pulse's class: short, medium or long, as their lengths fall.
fn class(cycles: u32) -> u8 {
    match cycles {
        0..=455 => b'S',
        456..=617 => b'M',
        _ => b'L',
    }
}

/// The blocks the pulses spell: each leader's length and the bytes after it.
fn blocks(tape: &Tape) -> Vec<(usize, Vec<u8>)> {
    let kinds: Vec<u8> = tape.pulses().iter().map(|&p| class(p)).collect();
    let mut blocks = Vec::new();
    let mut at = 0;
    while at < kinds.len() {
        let leader = kinds[at..].iter().take_while(|&&k| k == b'S').count();
        at += leader;
        let mut bytes = Vec::new();
        while at + 1 < kinds.len() && kinds[at] == b'L' && kinds[at + 1] == b'M' {
            at += 2;
            let mut byte = 0u8;
            let mut ones = 0;
            for bit in 0..9 {
                let one = match (kinds[at], kinds[at + 1]) {
                    (b'S', b'M') => 0,
                    (b'M', b'S') => 1,
                    pair => panic!("no bit at pulse {at}: {pair:?}"),
                };
                at += 2;
                if bit < 8 {
                    byte |= one << bit;
                }
                ones += one;
            }
            assert_eq!(ones % 2, 1, "odd parity at pulse {at}");
            bytes.push(byte);
        }
        if at + 1 < kinds.len() && kinds[at] == b'L' && kinds[at + 1] == b'S' {
            at += 2;
        }
        if bytes.is_empty() {
            break;
        }
        blocks.push((leader, bytes));
    }
    blocks
}

#[test]
fn save_records_the_original_s_format_pulse_for_pulse() {
    let tape = recorded(&saving());
    assert_eq!(
        tape.pulses().len(),
        41_634,
        "as many pulses as the original's recording of it"
    );
    for &pulse in tape.pulses() {
        assert!(
            (370..=392).contains(&pulse)
                || (525..=545).contains(&pulse)
                || (692..=712).contains(&pulse),
            "a pulse of {pulse} cycles"
        );
    }
    let blocks = blocks(&tape);
    let leaders: Vec<usize> = blocks.iter().map(|&(leader, _)| leader).collect();
    assert_eq!(leaders, [27_136, 79, 5_376, 79]);
    let countdown = |first: u8| (0..9).map(|step| first - step).collect::<Vec<u8>>();
    let mut header = Vec::from(*b"\x01\x01\x08\x0D\x08AB");
    header.resize(192, b' ');
    for (copy, first) in [(0, 0x89), (1, 0x09)] {
        let (_, bytes) = &blocks[copy];
        assert_eq!(bytes[..9], countdown(first)[..]);
        assert_eq!(bytes[9..201], header[..], "the header");
        assert_eq!(
            bytes[201],
            header.iter().fold(0, |sum, &b| sum ^ b),
            "its checksum"
        );
        let (_, bytes) = &blocks[2 + copy];
        assert_eq!(bytes[..9], countdown(first)[..]);
        assert_eq!(bytes[9..21], PROGRAM[..], "the program");
        assert_eq!(bytes[21], PROGRAM.iter().fold(0, |sum, &b| sum ^ b));
    }
}

/// [`PROGRAM`] loaded, or verified with `verify` 1, from the tape at `$4000`
/// with secondary address 0, its outcome at `$C001`.
fn loading(verify: u8) -> Vec<u8> {
    program(move |a| {
        if verify != 0 {
            for (offset, &value) in (0..).zip(&PROGRAM) {
                a.lda(imm(value ^ u8::from(offset == 5)));
                a.sta(abs(0x4000 + offset));
            }
        }
        setnam(a, b"AB");
        setlfs(a, 1, 1, 0);
        a.lda(imm(verify));
        a.ldx(imm(0x00));
        a.ldy(imm(0x40));
        a.jsr(entry::LOAD);
        a.php();
        a.stx(abs(0xC002));
        a.sty(abs(0xC003));
        a.pla();
        a.sta(abs(0xC001));
        a.lda(abs(zp::STATUS));
        a.sta(abs(0xC004));
    })
}

fn played(basic: &[u8], tape: Tape) -> Machine {
    let mut machine = settled(Standard::Pal, basic);
    machine.insert_tape(tape);
    machine.press_play(false);
    run(&mut machine, 60);
    machine
}

#[test]
fn load_reads_what_save_recorded_where_it_is_told() {
    let machine = played(&loading(0), recorded(&saving()));
    assert_eq!(machine.ram(0xC001) & 0x01, 0, "loaded");
    assert_eq!(machine.ram(0xC004), 0, "with no error");
    assert_eq!(
        (machine.ram(0xC002), machine.ram(0xC003)),
        (0x0C, 0x40),
        "the end, relocated"
    );
    let loaded: Vec<u8> = (0..12).map(|offset| machine.ram(0x4000 + offset)).collect();
    assert_eq!(loaded, PROGRAM);
}

#[test]
fn verify_reports_a_byte_that_differs() {
    let machine = played(&loading(1), recorded(&saving()));
    assert_eq!(machine.ram(0xC001) & 0x01, 0);
    assert_eq!(machine.ram(0xC004), 0x10, "the byte changed in memory");
}

#[test]
fn a_data_file_s_last_byte_carries_its_end() {
    let writing = program(|a| {
        setnam(a, b"SQ");
        setlfs(a, 1, 1, 1);
        a.jsr(entry::OPEN);
        a.ldx(imm(1));
        a.jsr(entry::CHKOUT);
        a.ldx(imm(0));
        a.here("byte");
        a.txa();
        a.ora(imm(1));
        a.jsr(entry::CHROUT);
        a.inx();
        a.bne("byte");
        a.jsr(entry::CLRCHN);
        a.lda(imm(1));
        a.jsr(entry::CLOSE);
        a.php();
        a.pla();
        a.sta(abs(0xC001));
    });
    let tape = recorded(&writing);
    let reading = program(|a| {
        setnam(a, b"SQ");
        setlfs(a, 1, 1, 0);
        a.jsr(entry::OPEN);
        a.ldx(imm(1));
        a.jsr(entry::CHKIN);
        a.ldy(imm(0));
        a.here("byte");
        a.jsr(entry::CHRIN);
        a.sta(abs(0xC100).y());
        a.lda(abs(zp::STATUS));
        a.sta(abs(0xC200).y());
        a.iny();
        a.and(imm(0x40));
        a.beq("byte");
        a.sty(abs(0xC002));
        a.jsr(entry::CLRCHN);
        a.lda(imm(1));
        a.jsr(entry::CLOSE);
    });
    let machine = played(&reading, tape);
    assert_eq!(machine.ram(0xC002), 0, "all 256 bytes, the count wrapping");
    let read: Vec<u8> = (0..=255u16).map(|i| machine.ram(0xC100 + i)).collect();
    let expected: Vec<u8> = (0..=255u8).map(|x| x | 1).collect();
    assert_eq!(read, expected);
    assert_eq!(machine.ram(0xC2FE), 0, "the last but one byte has no end");
    assert_eq!(machine.ram(0xC2FF), 0x40, "the last one does");
}

#[test]
fn found_waits_for_the_commodore_key() {
    let directly = program(|a| {
        a.lda(imm(0x80));
        a.jsr(entry::SETMSG);
        setnam(a, b"AB");
        setlfs(a, 1, 1, 0);
        a.lda(imm(0));
        a.ldx(imm(0x00));
        a.ldy(imm(0x40));
        a.jsr(entry::LOAD);
    });
    let mut machine = settled(Standard::Pal, &directly);
    machine.insert_tape(recorded(&saving()));
    machine.press_play(false);
    let shown = |machine: &Machine, text: &[u8]| {
        (0..25u16).any(|row| {
            let line: Vec<u8> = (0..40)
                .map(|column| machine.ram(0x0400 + row * 40 + column))
                .collect();
            line.windows(text.len()).any(|window| window == text)
        })
    };
    let found: &[u8] = b"\x06\x0F\x15\x0E\x04";
    let loading: &[u8] = b"\x0C\x0F\x01\x04\x09\x0E\x07";
    let mut seconds = 0;
    while !shown(&machine, found) {
        run_for(&mut machine, JIFFY);
        seconds += 1;
        assert!(seconds < 60 * 30, "FOUND came");
    }
    run_for(&mut machine, 60 * JIFFY);
    assert!(!shown(&machine, loading), "still waiting a second on");
    press(&mut machine, &[], Key::COMMODORE);
    assert!(
        shown(&machine, loading),
        "LOADING as the Commodore key goes down"
    );
}
