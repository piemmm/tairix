//! The keyboard against what the original's produced: each key's code
//! alone and with each shift key, the highest key winning, the repeat,
//! SHIFT with the Commodore key changing the character set, and a scan with
//! no key held.

use alloc::vec::Vec;

use super::{basic_rom, press, program, run_for, settled, JIFFY};
use crate::machine::c64::keyboard::Key;
use crate::machine::c64::{Machine, Standard};
use crate::machine::LetterCase;
use crate::rom::asm::{abs, hi, imm, lo};
use crate::rom::c64::kernal::fixed;
use crate::rom::c64::kernal::ws::{page, zp};

/// Where the recorder keeps each key taken from the buffer, the jiffy it
/// came at, their count, and the VIC-II's memory register.
const KEYS: u16 = 0xC100;
const TIMES: u16 = 0xC200;
const COUNT: u16 = 0xC0FF;
const MEMORY: u16 = 0xC0FE;

/// Where [`keylog_counter`]'s hook counts KEYLOG's calls.
const CALLS: u16 = 0xC0FD;

/// A BASIC ROM that counts KEYLOG's calls at [`CALLS`], passing each on to
/// the default, having set the repeat's counters, LSTX and LSTSHF to values
/// no scan leaves.
fn keylog_counter() -> Vec<u8> {
    basic_rom(|a| {
        a.sei();
        a.lda(imm(lo("hook")));
        a.sta(abs(page::KEYLOG));
        a.lda(imm(hi("hook")));
        a.sta(abs(page::KEYLOG + 1));
        for (addr, value) in [
            (CALLS, 0),
            (page::DELAY, 3),
            (page::KOUNT, 2),
            (zp::LSTX, 5),
            (page::LSTSHF, 7),
        ] {
            a.lda(imm(value));
            a.sta(abs(addr));
        }
        a.cli();
        a.here("idle");
        a.jmp("idle");
        a.here("hook");
        a.inc(abs(CALLS));
        a.jmp(abs(fixed::KEYLOG));
    })
}

/// A BASIC ROM that empties the keyboard buffer into [`KEYS`] as keys come.
fn recorder() -> Vec<u8> {
    basic_rom(|a| {
        a.ldx(imm(0));
        a.stx(abs(COUNT));
        a.here("wait");
        a.lda(abs(0xD018));
        a.sta(abs(MEMORY));
        a.lda(abs(zp::NDX));
        a.beq("wait");
        a.lda(abs(0x0277));
        a.sta(abs(KEYS).x());
        a.lda(abs(zp::TIME + 2));
        a.sta(abs(TIMES).x());
        a.inx();
        a.stx(abs(COUNT));
        a.lda(imm(0));
        a.sta(abs(zp::NDX));
        a.jmp("wait");
    })
}

fn recorded(machine: &Machine, from: u8) -> Vec<(u8, u8)> {
    (from..machine.ram(COUNT))
        .map(|i| {
            (
                machine.ram(KEYS + u16::from(i)),
                machine.ram(TIMES + u16::from(i)),
            )
        })
        .collect()
}

fn codes(machine: &mut Machine, held: &[Key], keys: &[Key]) -> Vec<u8> {
    keys.iter()
        .map(|&key| {
            let before = machine.ram(COUNT);
            press(machine, held, key);
            recorded(machine, before)
                .first()
                .map_or(0xFF, |&(code, _)| code)
        })
        .collect()
}

#[test]
fn keys_decode_as_the_original_s_tables_give_them() {
    let mut machine = settled(Standard::Pal, &recorder());
    let keys = [
        Key::A,
        Key::DIGIT_1,
        Key::AT,
        Key::POUND,
        Key::RUN_STOP,
        Key::DELETE,
        Key::F1,
        Key::SPACE,
    ];
    assert_eq!(
        codes(&mut machine, &[], &keys),
        [0x41, 0x31, 0x40, 0x5C, 0x03, 0x14, 0x85, 0x20]
    );
    assert_eq!(
        codes(&mut machine, &[Key::LEFT_SHIFT], &keys),
        [0xC1, 0x21, 0xBA, 0xA9, 0x83, 0x94, 0x89, 0xA0]
    );
    assert_eq!(
        codes(&mut machine, &[Key::COMMODORE], &keys),
        [0xB0, 0x81, 0xA4, 0xA8, 0x83, 0x94, 0x89, 0xA0]
    );
    assert_eq!(
        codes(&mut machine, &[Key::CONTROL], &keys),
        [0x01, 0x90, 0x00, 0x1C, 0xFF, 0xFF, 0xFF, 0xFF],
        "CTRL's table, which gives @ a code of 0 and the others none"
    );
}

#[test]
fn the_highest_key_held_is_the_one_taken() {
    let mut machine = settled(Standard::Pal, &recorder());
    machine.set_key(Key::A, true);
    machine.set_key(Key::S, true);
    run_for(&mut machine, 3 * JIFFY);
    machine.set_key(Key::A, false);
    machine.set_key(Key::S, false);
    run_for(&mut machine, 3 * JIFFY);
    assert_eq!(
        recorded(&machine, 0)
            .iter()
            .map(|&(code, _)| code)
            .collect::<Vec<_>>(),
        [0x53]
    );
    let before = machine.ram(COUNT);
    machine.set_key(Key::A, true);
    run_for(&mut machine, 3 * JIFFY);
    machine.set_key(Key::S, true);
    run_for(&mut machine, 3 * JIFFY);
    machine.set_key(Key::S, false);
    run_for(&mut machine, 3 * JIFFY);
    machine.set_key(Key::A, false);
    run_for(&mut machine, 3 * JIFFY);
    assert_eq!(
        recorded(&machine, before)
            .iter()
            .map(|&(code, _)| code)
            .collect::<Vec<_>>(),
        [0x41, 0x53, 0x41],
        "A, S while it is held over A, then A again"
    );
}

#[test]
fn the_cursor_keys_the_space_bar_and_delete_repeat() {
    let mut machine = settled(Standard::Pal, &recorder());
    for (key, repeats) in [
        (Key::CURSOR_RIGHT, true),
        (Key::SPACE, true),
        (Key::DELETE, true),
        (Key::A, false),
        (Key::RETURN, false),
    ] {
        let before = machine.ram(COUNT);
        machine.set_key(key, true);
        run_for(&mut machine, 60 * JIFFY);
        machine.set_key(key, false);
        run_for(&mut machine, 3 * JIFFY);
        let times = recorded(&machine, before);
        let first = times[0].1;
        let gaps: Vec<u8> = times
            .iter()
            .map(|&(_, at)| at.wrapping_sub(first))
            .collect();
        if repeats {
            assert_eq!(
                gaps,
                [0, 19, 23, 27, 31, 35, 39, 43, 47, 51, 55, 59],
                "{key:?}"
            );
        } else {
            assert_eq!(gaps, [0], "{key:?}");
        }
    }
}

#[test]
fn shift_and_the_commodore_key_change_the_character_set() {
    let mut machine = settled(Standard::Pal, &recorder());
    let set = |machine: &Machine| (machine.ram(MEMORY) & 0x02, machine.letter_case());
    assert_eq!(set(&machine), (0, LetterCase::CapitalsOnly), "upper case");
    let before = machine.ram(COUNT);
    press(&mut machine, &[Key::LEFT_SHIFT], Key::COMMODORE);
    assert_eq!(set(&machine), (0x02, LetterCase::Small), "lower case");
    press(&mut machine, &[Key::LEFT_SHIFT, Key::COMMODORE], Key::A);
    assert_eq!(
        set(&machine),
        (0, LetterCase::CapitalsOnly),
        "upper case again, and no key"
    );
    assert_eq!(machine.ram(COUNT), before);
}

#[test]
fn a_scan_with_no_key_held_calls_no_keylog_and_leaves_the_repeat_alone() {
    let mut machine = settled(Standard::Pal, &keylog_counter());
    run_for(&mut machine, 10 * JIFFY);
    assert_eq!(machine.ram(CALLS), 0, "no key, no KEYLOG");
    assert_eq!(
        [page::DELAY, page::KOUNT, zp::LSTX, page::LSTSHF].map(|addr| machine.ram(addr)),
        [3, 2, 64, 0],
        "the counters left, the last key and shift keys cleared"
    );
    machine.set_key(Key::LEFT_SHIFT, true);
    run_for(&mut machine, 3 * JIFFY);
    assert!(machine.ram(CALLS) >= 2, "SHIFT alone is decoded");
}

#[test]
fn cint_leaves_the_first_repeat_s_delay_at_ten_jiffies() {
    let machine = settled(Standard::Pal, &program(|_| {}));
    assert_eq!(machine.ram(page::DELAY), 10);
    assert_eq!(machine.ram(page::KOUNT), 4);
}
