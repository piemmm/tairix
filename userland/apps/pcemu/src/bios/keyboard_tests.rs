use super::super::run_until;
use super::super::tests::{booted, call, poke, run, start, stopped_at, wait_ticks};
use crate::asm::{Reg16, Reg8};
use crate::chips::keyboard::Scan;
use crate::cpu::{flag, Bus};
use crate::machine::{Machine, Template};
use crate::time::NANOS_PER_SECOND;
use alloc::vec;

fn xt() -> Machine {
    booted(Template::IbmXt, None)
}

/// Taps each of `keys` with the `held` keys down round them.
fn chord(m: &mut Machine, held: &[u8], keys: &[u8]) {
    for &scan in held {
        m.board_mut().key(Scan::plain(scan), true);
    }
    for &scan in keys {
        m.board_mut().key(Scan::plain(scan), true);
        m.board_mut().key(Scan::plain(scan), false);
    }
    for &scan in held.iter().rev() {
        m.board_mut().key(Scan::plain(scan), false);
    }
}

fn read_key(m: &mut Machine) -> u16 {
    call(m, 0x16, |r| r.set16(0, 0x0000)).get16(0)
}

/// Lets the BIOS take what the keyboard has sent.
fn settle(m: &mut Machine) {
    run(m, |asm| wait_ticks(asm, 2), |_| {});
}

/// A handler for INT `vector` that marks `0:05F0h` and returns.
fn mark_on(m: &mut Machine, vector: u32) {
    poke(m, 0x0700, &[0xC6, 0x06, 0xF0, 0x05, 0x01, 0xCF]);
    poke(m, vector * 4, &[0x00, 0x07, 0x00, 0x00]);
}

#[test]
fn shift_and_caps_lock_choose_a_letters_case() {
    let mut m = xt();
    chord(&mut m, &[], &[0x1E]);
    assert_eq!(read_key(&mut m), 0x1E61);
    chord(&mut m, &[0x2A], &[0x1E]);
    assert_eq!(read_key(&mut m), 0x1E41);
    chord(&mut m, &[], &[0x3A, 0x1E]);
    assert_eq!(read_key(&mut m), 0x1E41, "Caps Lock");
    assert_ne!(m.board().peek(0x417) & 0x40, 0);
    chord(&mut m, &[0x36], &[0x1E, 0x02]);
    assert_eq!(read_key(&mut m), 0x1E61, "Shift undoes it for a letter");
    assert_eq!(read_key(&mut m), 0x0221, "but not for a digit");
}

#[test]
fn alt_and_the_keypad_type_a_code() {
    let mut m = xt();
    chord(&mut m, &[0x38], &[0x4D, 0x4C]);
    assert_eq!(read_key(&mut m), 0x0041, "65 is A");
}

#[test]
fn ctrl_break_empties_the_buffer_and_calls_int_1bh() {
    let mut m = xt();
    mark_on(&mut m, 0x1B);
    chord(&mut m, &[], &[0x1E, 0x30]);
    chord(&mut m, &[0x1D], &[0x46]);
    settle(&mut m);
    assert_eq!(
        read_key(&mut m),
        0x0000,
        "only the break's own code is left"
    );
    assert_eq!(m.board().peek(0x471), 0x80);
    assert_eq!(m.board().peek(0x5F0), 1, "INT 1Bh ran");
}

#[test]
fn shift_and_print_screen_call_int_05h() {
    let mut m = xt();
    mark_on(&mut m, 0x05);
    chord(&mut m, &[0x2A], &[0x37]);
    settle(&mut m);
    assert_eq!(m.board().peek(0x5F0), 1);
    let waiting = call(&mut m, 0x16, |r| r.set16(0, 0x0100));
    assert!(waiting.flag(flag::ZF), "no key was stored");
}

#[test]
fn a_full_buffer_takes_no_more() {
    let mut m = xt();
    for n in 0..15u16 {
        let stored = call(&mut m, 0x16, |r| {
            r.set16(0, 0x0500);
            r.set16(1, 0x1E61 + n);
        });
        assert_eq!(stored.get8(0), 0, "key {n} stored");
    }
    let refused = call(&mut m, 0x16, |r| {
        r.set16(0, 0x0500);
        r.set16(1, 0x3062);
    });
    assert_eq!(refused.get8(0), 1, "the sixteenth is refused");
    chord(&mut m, &[], &[0x30]);
    settle(&mut m);
    for n in 0..15u16 {
        assert_eq!(read_key(&mut m), 0x1E61 + n);
    }
    let waiting = call(&mut m, 0x16, |r| r.set16(0, 0x0100));
    assert!(
        waiting.flag(flag::ZF),
        "the key typed into a full buffer was dropped"
    );
}

#[test]
fn ctrl_alt_del_restarts_the_machine_keeping_its_memory() {
    let mut m = xt();
    poke(&mut m, 0x9000, b"kept");
    poke(&mut m, 0xB_8000 + 10 * 160, b"Q");
    chord(&mut m, &[0x1D, 0x38], &[0x53]);
    start(&mut m, |asm| wait_ticks(asm, 4), |_| {});
    let now = m.now();
    run_until(&mut m, now + 3 * NANOS_PER_SECOND);
    assert!(stopped_at(&m, 0, 0x7C00..0x7E00), "booted again");
    let kept: [u8; 4] =
        core::array::from_fn(|i| m.board().peek(0x9000 + u32::try_from(i).unwrap()));
    assert_eq!(&kept, b"kept", "a warm start clears no memory");
    assert_ne!(
        m.board().peek(0xB_8000 + 10 * 160),
        b'Q',
        "the screen was set again"
    );
}

#[test]
fn the_key_each_character_is_typed_with_makes_that_character() {
    for byte in (0x20u8..0x7F).chain([b'\r', b'\t', 0x08, 0x1B]) {
        let (key, shifted) = crate::chips::keyboard::us_key(byte)
            .unwrap_or_else(|| panic!("no key types {byte:02X}"));
        let made = super::KEYS[usize::from(key.code)][usize::from(shifted)];
        assert_eq!(
            made.to_le_bytes()[0],
            byte,
            "{byte:02X} through {:02X}",
            key.code
        );
    }
    assert_eq!(crate::chips::keyboard::us_key(0x7F), None);
    assert_eq!(
        crate::chips::keyboard::us_key(b'\n'),
        None,
        "line feed is no key's"
    );
}

/// The deepest the stack below `0:7000h` reached since it was filled with
/// `fill`: how many bytes from its top were written.
fn stack_reach(m: &Machine, fill: u8) -> u32 {
    (0x6000u32..0x7000)
        .find(|&at| m.board().peek(at) != fill)
        .map_or(0, |at| 0x7000 - at)
}

#[test]
fn keys_pressed_into_a_full_buffer_never_nest_the_handler() {
    // The handler waited out its beep with the keyboard's interrupt let
    // in, so each key pressed meanwhile beeped again a handler deeper, on
    // whatever stack the machine was using, until it ran out.
    let mut m = xt();
    chord(&mut m, &[], &[0x1E; 15]);
    poke(&mut m, 0x6000, &[0xEE; 0x1000]);
    let end = start(&mut m, |asm| wait_ticks(asm, 12), |_| {});
    for _ in 0..20 {
        for down in [true, false] {
            let now = m.now();
            run_until(&mut m, now + NANOS_PER_SECOND / 50);
            m.board_mut().key(Scan::plain(0x30), down);
        }
    }
    let begun = m.now();
    while !stopped_at(&m, 0, end.clone()) {
        assert!(m.now() - begun < 5 * NANOS_PER_SECOND, "the program ended");
        let now = m.now();
        run_until(&mut m, now + NANOS_PER_SECOND / 20);
    }
    let reach = stack_reach(&m, 0xEE);
    assert!(reach < 0x80, "one handler at a time: {reach} bytes deep");
    assert_eq!(
        m.board_mut().input(0x21, 0) & 0x02,
        0,
        "the keyboard's line is open again"
    );
    assert_eq!(m.board_mut().input(0x61, 0) & 0x03, 0, "the beep is over");
    assert_eq!(read_key(&mut m), 0x1E61, "the buffer kept what it held");
}

#[test]
fn a_pause_ended_returns_whatever_its_stack_held() {
    // The state the handler keeps on the stack was read before it was
    // written: ending a pause took the stale bytes there for a beep that
    // would end a quarter of an hour on.
    let mut m = xt();
    // A program that keeps the stack below it full of such bytes and takes
    // interrupts only once it has filled it again.
    start(
        &mut m,
        |asm| {
            let again = asm.label();
            asm.bind(&again);
            asm.cli();
            asm.cld();
            asm.zero(Reg16::Ax);
            asm.bytes(&[0x8E, 0xC0]); // mov es, ax
            asm.mov_imm16(Reg16::Di, 0x6F00);
            asm.mov_imm16(Reg16::Cx, 0x100);
            asm.mov_imm8(Reg8::Al, 0x40);
            asm.bytes(&[0xF3, 0xAA]); // rep stosb
            asm.sti();
            asm.bytes(&[0x90, 0x90]); // nop; nop
            asm.jmp_short(&again);
        },
        |_| {},
    );
    let in_program = |m: &Machine| m.cpu().regs().sreg(crate::cpu::Seg::Cs) == 0;
    chord(&mut m, &[0x1D], &[0x45]);
    let now = m.now();
    run_until(&mut m, now + NANOS_PER_SECOND / 2);
    assert!(!in_program(&m), "paused");
    chord(&mut m, &[], &[0x1E]);
    let resumed = m.now();
    while !in_program(&m) {
        assert!(
            m.now() - resumed < 2 * NANOS_PER_SECOND,
            "the program went on once the pause ended"
        );
        let now = m.now();
        run_until(&mut m, now + NANOS_PER_SECOND / 20);
    }
}

#[test]
fn a_key_pressed_while_the_board_leads_the_cpu_is_taken() {
    // A write at the next cycle leaves the board's clock a part of a cycle
    // ahead of the CPU's; a key pressed then waited behind an event already
    // past, which held the CPU's limit behind it, and no time passed.
    let mut m = xt();
    let end = start(&mut m, |asm| wait_ticks(asm, 1), |_| {});
    chord(&mut m, &[], &[0x1E]);
    let begun = m.now();
    run_until(&mut m, begun + NANOS_PER_SECOND);
    assert!(m.now() >= begun + NANOS_PER_SECOND, "time went on");
    assert!(stopped_at(&m, 0, end), "the program ran to its end");
    assert_eq!(read_key(&mut m), 0x1E61);
}

#[test]
fn a_code_no_key_types_goes_on_the_keypad_under_alt_without_leading_zeros() {
    let (alt, shift) = (Scan::plain(0x38), crate::chips::keyboard::SHIFT);
    let tap = |code: u8| [(Scan::plain(code), true), (Scan::plain(code), false)];
    let strokes = super::keystrokes(0x82, false);
    let mut expected = vec![(alt, true)];
    expected.extend(tap(0x4F));
    expected.extend(tap(0x51));
    expected.extend(tap(0x52));
    expected.push((alt, false));
    assert_eq!(
        strokes.as_slice(),
        expected,
        "130 on the keypad's 1, 3 and 0"
    );
    let mut seven = vec![(alt, true)];
    seven.extend(tap(0x47));
    seven.push((alt, false));
    assert_eq!(super::keystrokes(0x07, false).as_slice(), seven);
    assert!(super::keystrokes(0, false).as_slice().is_empty());
    let mut shifted = vec![(shift, true)];
    shifted.extend(tap(0x1E));
    shifted.push((shift, false));
    assert_eq!(
        super::keystrokes(b'a', true).as_slice(),
        shifted,
        "Caps Lock on"
    );
    assert_eq!(
        super::keystrokes(b'1', true).as_slice(),
        tap(0x02),
        "not a letter"
    );
}

#[test]
fn every_code_typed_is_the_code_read_with_caps_lock_either_way() {
    let mut m = xt();
    for caps in [false, true] {
        if caps {
            chord(&mut m, &[], &[0x3A]);
            settle(&mut m);
            assert!(super::caps_lock(&m));
        }
        for code in (0..=u8::MAX).filter(|&code| crate::link::typeable(code)) {
            for &(key, down) in super::keystrokes(code, caps).as_slice() {
                m.board_mut().key(key, down);
            }
            assert_eq!(
                read_key(&mut m).to_le_bytes()[0],
                code,
                "{code:#04x}, Caps Lock {caps}"
            );
        }
    }
}

#[test]
fn the_keys_waiting_are_counted_only_in_a_ring_a_program_could_walk() {
    let mut m = xt();
    assert_eq!(super::keys_waiting(&m), Some(0));
    chord(&mut m, &[], &[0x1E, 0x30, 0x2E]);
    settle(&mut m);
    assert_eq!(super::keys_waiting(&m), Some(3));
    read_key(&mut m);
    assert_eq!(super::keys_waiting(&m), Some(2));
    poke(&mut m, 0x482, &[0x7F, 0x00]);
    assert_eq!(super::keys_waiting(&m), None, "an end below the start");
}
