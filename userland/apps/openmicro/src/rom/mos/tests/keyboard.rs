//! The keyboard as MOS 1.20 reads it: what the lock keys do, what SHIFT and
//! CTRL do with them, what the status byte records, and which keys repeat.
//! Each expectation is what MOS 1.20 gave for the same keys.

use alloc::vec::Vec;

use super::{boot_rom, var};
use crate::machine::bbc::keyboard::Key;
use crate::machine::bbc::Machine;
use crate::rom::asm::{abs, imm, zp};
use crate::rom::os::entry::{OSBYTE, OSRDCH};
use crate::scenario::{press, press_with, wait};

/// Where the reader keeps each code OSRDCH returns, and how many.
const RECEIVED: u16 = 0x0900;
const COUNT: u16 = 0x0074;
/// How long the reader takes to be reading, in centiseconds.
const STARTED: u64 = 100;
/// A key let go and the next pressed, in centiseconds, as a held key is.
const SETTLE: u64 = 3;
/// The OSBYTE calls that set the keyboard status and the function keys'
/// codes, unshifted and with SHIFT and CTRL.
const KEYBOARD_STATUS: u8 = 202;
const FUNCTION_KEYS: u8 = 225;
const SHIFT_CTRL_FUNCTION_KEYS: u8 = 228;

/// A machine whose language makes the OSBYTE calls `settings`, each A and
/// X, then keeps every code OSRDCH returns.
fn reader(settings: &[(u8, u8)]) -> Machine {
    let rom = super::program(None, |a| {
        for &(call, x) in settings {
            a.lda(imm(call));
            a.ldx(imm(x));
            a.ldy(imm(0));
            a.jsr(OSBYTE);
        }
        a.lda(imm(0));
        a.sta(zp(COUNT));
        let next = a.here_local();
        a.jsr(OSRDCH);
        a.ldx(zp(COUNT));
        a.sta(abs(RECEIVED).x());
        a.inc(zp(COUNT));
        a.jmp(next);
    });
    let mut machine = boot_rom(&rom);
    wait(&mut machine, STARTED);
    machine
}

fn received(machine: &Machine) -> Vec<u8> {
    (RECEIVED..)
        .take(usize::from(machine.peek(COUNT)))
        .map(|address| machine.peek(address))
        .collect()
}

fn status(machine: &Machine) -> u8 {
    machine.peek(var::KEYBOARD_STATUS)
}

/// `key` pressed while `held` is down, `held` pressed first and let go last.
fn under(machine: &mut Machine, held: Key, key: Key) {
    machine.set_key(held, true);
    wait(machine, SETTLE);
    press(machine, key);
    machine.set_key(held, false);
    wait(machine, SETTLE);
}

/// A, 1 and :, each alone and with SHIFT.
fn type_pairs(machine: &mut Machine) {
    for key in [Key::A, Key::DIGIT_1, Key::COLON] {
        press(machine, key);
        press_with(machine, key, &[Key::SHIFT]);
    }
}

#[test]
fn caps_lock_types_capitals_whether_or_not_shift_is_held() {
    let mut machine = reader(&[]);
    assert_eq!(status(&machine), 0x20);
    type_pairs(&mut machine);
    assert_eq!(received(&machine), b"AA1!:*");
}

#[test]
fn caps_lock_turns_over_and_turns_shift_lock_off() {
    let mut machine = reader(&[]);
    press(&mut machine, Key::CAPS_LOCK);
    assert_eq!(status(&machine), 0x30);
    type_pairs(&mut machine);
    assert_eq!(received(&machine), b"aA1!:*");
    press(&mut machine, Key::CAPS_LOCK);
    assert_eq!(status(&machine), 0x20);
    press(&mut machine, Key::SHIFT_LOCK);
    press(&mut machine, Key::CAPS_LOCK);
    assert_eq!(status(&machine), 0x20);
}

#[test]
fn shift_with_caps_lock_makes_shift_type_lower_case() {
    let mut machine = reader(&[]);
    under(&mut machine, Key::SHIFT, Key::CAPS_LOCK);
    assert_eq!(status(&machine), 0xA8, "SHIFT recorded as it was");
    under(&mut machine, Key::SHIFT, Key::CAPS_LOCK);
    assert_eq!(status(&machine), 0xA8, "a second time changes nothing");
    type_pairs(&mut machine);
    assert_eq!(received(&machine), b"Aa1!:*");
    assert_eq!(status(&machine), 0xA0);
    press(&mut machine, Key::CAPS_LOCK);
    assert_eq!(status(&machine), 0x30, "CAPS LOCK alone ends it");
}

#[test]
fn shift_lock_shifts_every_key_and_turns_caps_lock_off() {
    let mut machine = reader(&[]);
    press(&mut machine, Key::SHIFT_LOCK);
    assert_eq!(status(&machine), 0x10);
    type_pairs(&mut machine);
    assert_eq!(received(&machine), b"AA!!**");
    press(&mut machine, Key::SHIFT_LOCK);
    assert_eq!(status(&machine), 0x30, "both locks off");
    under(&mut machine, Key::SHIFT, Key::CAPS_LOCK);
    press(&mut machine, Key::SHIFT_LOCK);
    assert_eq!(status(&machine), 0x10, "and SHIFT reversing them ends");
}

#[test]
fn while_shift_reverses_the_locks_shift_undoes_shift_lock() {
    let mut machine = reader(&[(KEYBOARD_STATUS, 0x90)]);
    type_pairs(&mut machine);
    assert_eq!(received(&machine), b"Aa!1*:");
}

#[test]
fn the_lock_keys_do_not_repeat_as_other_keys_do() {
    for (key, toggled) in [(Key::CAPS_LOCK, 0x30), (Key::SHIFT_LOCK, 0x10)] {
        let mut machine = reader(&[]);
        machine.set_key(key, true);
        wait(&mut machine, 100);
        machine.set_key(key, false);
        wait(&mut machine, SETTLE);
        assert_eq!(status(&machine), toggled, "{}", key.label());
    }
    let mut machine = reader(&[]);
    machine.set_key(Key::A, true);
    wait(&mut machine, 100);
    machine.set_key(Key::A, false);
    wait(&mut machine, SETTLE);
    assert!(received(&machine).len() > 2, "A repeats");
}

/// SHIFT and CTRL are recorded as each scan of the keyboard finds them,
/// while a key is down and as the last is let go.
#[test]
fn the_status_records_shift_and_ctrl_as_the_keyboard_is_scanned() {
    let mut machine = reader(&[]);
    machine.set_key(Key::A, true);
    wait(&mut machine, SETTLE);
    machine.set_key(Key::SHIFT, true);
    wait(&mut machine, 10);
    assert_eq!(status(&machine), 0x28);
    machine.set_key(Key::A, false);
    wait(&mut machine, SETTLE);
    machine.set_key(Key::SHIFT, false);
    wait(&mut machine, SETTLE);
    assert_eq!(status(&machine), 0x28, "SHIFT was down as A went up");
    under(&mut machine, Key::CTRL, Key::CAPS_LOCK);
    assert_eq!(status(&machine), 0x70);
    let mut machine = reader(&[(KEYBOARD_STATUS, 0x48)]);
    assert_eq!(status(&machine), 0x48);
    press(&mut machine, Key::A);
    assert_eq!(status(&machine), 0x00, "nothing held as A was scanned");
}

#[test]
fn ctrl_takes_the_code_shift_lock_gives() {
    let mut machine = reader(&[]);
    press(&mut machine, Key::SHIFT_LOCK);
    press_with(&mut machine, Key::DIGIT_1, &[Key::CTRL]);
    press_with(&mut machine, Key::A, &[Key::CTRL]);
    assert_eq!(received(&machine), [b'!', 0x01]);
}

#[test]
fn shift_lock_shifts_the_function_keys() {
    let settings = [(FUNCTION_KEYS, 0x80), (SHIFT_CTRL_FUNCTION_KEYS, 0xA0)];
    let mut machine = reader(&settings);
    press_with(&mut machine, Key::F1, &[Key::CTRL]);
    press(&mut machine, Key::SHIFT_LOCK);
    press_with(&mut machine, Key::F1, &[Key::CTRL]);
    assert_eq!(received(&machine), [0x91, 0xA1]);
}

/// The regression: a zone set to zero was not ignored when it held a
/// function key's codes, but gave each its number.
#[test]
fn a_function_key_zone_set_to_zero_ignores_its_keys() {
    let mut machine = reader(&[(FUNCTION_KEYS, 0)]);
    press(&mut machine, Key::F1);
    press_with(&mut machine, Key::F1, &[Key::SHIFT]);
    press(&mut machine, Key::A);
    assert_eq!(received(&machine), [0x81, b'A']);
}
