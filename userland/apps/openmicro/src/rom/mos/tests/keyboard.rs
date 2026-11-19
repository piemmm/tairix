//! The keyboard as MOS 1.20 reads it: what the lock keys do, what SHIFT and
//! CTRL do with them, what the status byte records, and which keys repeat.
//! Each expectation is what MOS 1.20 gave for the same keys.

use alloc::vec::Vec;

use super::{boot_offer_rom, boot_rom, page2, var, BOOT_OFFER};
use crate::machine::bbc::keyboard::Key;
use crate::machine::bbc::Machine;
use crate::rom::asm::{abs, imm, zp};
use crate::rom::mos::ws::os;
use crate::rom::os::entry::{OSBYTE, OSRDCH};
use crate::scenario::{press, press_break, press_with, wait};

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

/// A machine at its language's start, idle at a halt.
fn idle() -> Machine {
    let mut machine = boot_rom(&super::program(None, |_| {}));
    wait(&mut machine, STARTED);
    machine
}

/// Each value `at` takes over `centiseconds`, sampled every 50 cycles, and
/// when each is first seen.
fn values(machine: &mut Machine, at: u16, centiseconds: u64) -> Vec<(u64, u8)> {
    let end = machine.cycles() + centiseconds * 20_000;
    let mut seen = alloc::vec![(machine.cycles(), machine.peek(at))];
    while machine.cycles() < end {
        machine.run_until(machine.cycles() + 50);
        let value = machine.peek(at);
        if seen.last().is_some_and(|&(_, last)| last != value) {
            seen.push((machine.cycles(), value));
        }
    }
    seen
}

/// The regression: a key went straight to its number with the down bit set
/// and back to none, where MOS 1.20 shows its bare number from finding it
/// to the tick that confirms it, and for a tick after it is let go, which
/// games poll for.
#[test]
fn a_key_shows_its_bare_number_when_found_and_when_let_go() {
    let mut machine = idle();
    let space = Key::SPACE.number();
    let down = space | 0x80;
    machine.set_key(Key::SPACE, true);
    let pressed: Vec<u8> = values(&mut machine, os::KEY_LAST, 5)
        .iter()
        .map(|&(_, value)| value)
        .collect();
    assert_eq!(pressed, [0, space, down]);
    machine.set_key(Key::SPACE, false);
    let released: Vec<u8> = values(&mut machine, os::KEY_LAST, 5)
        .iter()
        .map(|&(_, value)| value)
        .collect();
    assert_eq!(released, [down, space, 0]);
}

/// The regression's whole: a loop polling &EC for SPACE's bare number, as
/// Citadel 2 waits to start, ends once SPACE is pressed and let go. Whether
/// the press itself shows long enough to be seen depends on where it falls
/// against the tick, on MOS 1.20 as here; the tick after a release always
/// leaves it there for one more.
#[test]
fn a_poll_for_space_s_bare_number_ends_once_it_is_pressed() {
    const SEEN: u16 = 0x0075;
    let space = Key::SPACE.number();
    let rom = super::program(None, |a| {
        a.lda(imm(0));
        a.sta(zp(SEEN));
        let poll = a.here_local();
        a.lda(zp(os::KEY_LAST));
        a.cmp(imm(space));
        a.bne(poll);
        a.sta(zp(SEEN));
    });
    let mut machine = boot_rom(&rom);
    wait(&mut machine, STARTED);
    press(&mut machine, Key::SPACE);
    wait(&mut machine, 3);
    assert_eq!(machine.peek(SEEN), space);
}

/// A key is typed a tick after the tick that confirms it, its count of 1
/// running out, as MOS 1.20 types it.
#[test]
fn a_key_is_typed_a_tick_after_it_is_confirmed() {
    let mut machine = idle();
    let typed = machine.peek(page2::MODEL_B.buffer_end);
    machine.set_key(Key::A, true);
    let last = values(&mut machine, os::KEY_LAST, 5);
    let confirmed = last
        .iter()
        .find(|&&(_, value)| value & 0x80 != 0)
        .expect("confirmed")
        .0;
    let start = last[0].0;
    machine.set_key(Key::A, false);
    let mut machine_again = idle();
    machine_again.set_key(Key::A, true);
    let buffer = values(&mut machine_again, page2::MODEL_B.buffer_end, 5);
    let first = buffer
        .iter()
        .find(|&&(_, value)| value != typed)
        .expect("typed")
        .0;
    let tick = 20_000;
    let (confirmed, first) = (confirmed - start, first - buffer[0].0);
    assert!(
        first > confirmed + tick * 9 / 10 && first < confirmed + tick * 11 / 10,
        "confirmed at {confirmed}, typed at {first}"
    );
}

/// Two keys held: the second is the last key and the first the first key,
/// which is forgotten as soon as it is let go; the last clears its down bit
/// a tick before it is forgotten.
#[test]
fn a_second_key_makes_the_first_the_first_key_until_it_is_let_go() {
    let mut machine = idle();
    let (a_key, s_key) = (Key::A.number(), Key::S.number());
    machine.set_key(Key::A, true);
    wait(&mut machine, 5);
    machine.set_key(Key::S, true);
    wait(&mut machine, 5);
    assert_eq!(machine.peek(os::KEY_FIRST), a_key | 0x80);
    assert_eq!(machine.peek(os::KEY_LAST), s_key | 0x80);
    machine.set_key(Key::A, false);
    let first: Vec<u8> = values(&mut machine, os::KEY_FIRST, 3)
        .iter()
        .map(|&(_, value)| value)
        .collect();
    assert_eq!(first, [a_key | 0x80, 0]);
    assert_eq!(machine.peek(os::KEY_LAST), s_key | 0x80);
}

/// With auto-repeat off, a held key is typed once.
#[test]
fn with_auto_repeat_off_a_held_key_is_typed_once() {
    const AUTO_REPEAT_DELAY: u8 = 11;
    let mut machine = reader(&[(AUTO_REPEAT_DELAY, 0)]);
    machine.set_key(Key::A, true);
    wait(&mut machine, 150);
    machine.set_key(Key::A, false);
    wait(&mut machine, SETTLE);
    assert_eq!(received(&machine), b"A");
}

/// The boot reads SHIFT from the keyboard status as it records SHIFT and CTRL
/// there, where they stay until the next scan; it offers the boot with Y 0
/// and Y 8 otherwise, the link reversing SHIFT-BREAK reversing the two, as
/// MOS 1.20 does.
#[test]
fn break_records_shift_and_ctrl_in_the_status_the_boot_reads() {
    const REVERSED: u8 = 0xF7;
    for (links, plain, shifted) in [(0xFF, 8, 0), (REVERSED, 0, 8)] {
        let mut machine = boot_rom(&boot_offer_rom());
        machine.set_startup_options(links);
        wait(&mut machine, STARTED);
        let offer = |machine: &Machine| (machine.peek(BOOT_OFFER), status(machine));
        assert_eq!(
            offer(&machine),
            (plain, 0x20),
            "power-on, links {links:02X}"
        );
        press_break(&mut machine, &[]);
        assert_eq!(offer(&machine), (plain, 0x20), "BREAK");
        press_break(&mut machine, &[Key::SHIFT]);
        assert_eq!(
            offer(&machine),
            (shifted, 0x28),
            "SHIFT-BREAK, SHIFT let go"
        );
        press(&mut machine, Key::A);
        assert_eq!(status(&machine), 0x20, "the next scan finds SHIFT up");
        press_break(&mut machine, &[Key::CTRL]);
        assert_eq!(offer(&machine), (plain, 0x60), "CTRL-BREAK");
    }
}
