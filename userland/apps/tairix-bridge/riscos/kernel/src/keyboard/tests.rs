//! The kernel's key path's tests, through a key handler in guest code
//! called as RISC OS calls one: its initialisation, its simple keys with
//! Shift and Caps Lock, its special and shifting keys' code, a pending Alt,
//! two keys rolled over, the repeat, the keys read by internal number, and
//! a keyboard's arrival.

use alloc::vec::Vec;

use tairix_riscos_arm::psr;

use crate::cli::tests::session;
use crate::msgtrans::tests::swi;
use crate::swi::os::{BYTE, CALL_A_VECTOR, INSTALL_KEY_HANDLER};
use crate::testkit::asm::{self, Condition};
use crate::testkit::{Session, DATA};
use crate::vectors;

/// `KeyV`.
const KEY_VECTOR: u32 = vectors::KEY.0;

/// The guest handler's header, and where its tables and code are from it.
const HANDLER: u32 = DATA + 0x1000;
const TRANSLATION: u32 = 0x40;
const INKEY: u32 = 0x80;
const SHIFTING: u32 = 0x300;
const SPECIALS: u32 = 0x310;
const CODES: u32 = 0x320;
const INIT: u32 = 0x340;
const PENDING_ALT: u32 = 0x360;
const SPECIAL_CODE: u32 = 0x380;
const SHIFT_CODE: u32 = 0x3A0;
const SPECIAL_LIST: u32 = 0x3C0;
const PENDING_LIST: u32 = 0x3D0;
/// Its keys: one simple, one special, one shifting.
const SIMPLE: u32 = 0;
const SPECIAL: u32 = 1;
const SHIFT: u32 = 2;
/// The internal key its table gives the simple key.
const SIMPLE_INTERNAL: u32 = 10;
/// The status its initialisation leaves: Caps Lock off, the first layer;
/// and its shifting key's, down and up: Caps Lock off, Shift held or not.
const STARTED: u8 = 0x30;
const SHIFT_DOWN: u8 = 0x18;
const SHIFT_UP: u8 = 0x10;

/// The guest handler installed: the one it replaced.
fn install(s: &mut Session) -> u32 {
    let h = HANDLER;
    s.poke(
        h,
        &[
            TRANSLATION,
            3,
            INKEY,
            SHIFTING,
            SPECIALS,
            CODES,
            INIT,
            PENDING_ALT,
        ],
    );
    let simple = u32::from_le_bytes([b'x', b'X', 0x18, 0x18]);
    s.poke(h + TRANSLATION, &[simple, u32::MAX, u32::MAX]);
    s.poke(h + INKEY, &[u32::MAX; 128]);
    s.poke(
        h + INKEY + 4 * (127 - SIMPLE_INTERNAL),
        &[0xFFFF_FF00 | SIMPLE],
    );
    s.write(h + SHIFTING, &[1, SHIFT.to_le_bytes()[0]]);
    s.write(
        h + SPECIALS,
        &[2, SPECIAL.to_le_bytes()[0], SHIFT.to_le_bytes()[0]],
    );
    s.poke(h + CODES, &[SPECIAL_CODE - CODES, SHIFT_CODE - CODES]);
    s.poke(
        h + INIT,
        &[asm::mov(5, u32::from(STARTED)), asm::mov_pc_lr()],
    );
    s.poke(
        h + PENDING_ALT,
        &[
            asm::adr(6, h + PENDING_ALT, h + PENDING_LIST),
            asm::mov_pc_lr(),
        ],
    );
    s.poke(
        h + SPECIAL_CODE,
        &[
            asm::adr(6, h + SPECIAL_CODE, h + SPECIAL_LIST),
            asm::mov_pc_lr(),
        ],
    );
    s.poke(
        h + SHIFT_CODE,
        &[
            asm::cmp_imm(1, 1),
            asm::when(Condition::Eq, asm::mov(5, u32::from(SHIFT_DOWN))),
            asm::when(Condition::Ne, asm::mov(5, u32::from(SHIFT_UP))),
            asm::mov_pc_lr(),
        ],
    );
    s.write(h + SPECIAL_LIST, &[2, b'p', b'q']);
    s.write(h + PENDING_LIST, &[1, b'!']);
    swi(
        s,
        INSTALL_KEY_HANDLER,
        [h, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("installed")
    .r[0]
}

/// Low-level key `low` going down or up, through `KeyV`.
fn press(s: &mut Session, low: u32, down: bool) {
    let reason = if down { 2 } else { 1 };
    swi(
        s,
        CALL_A_VECTOR,
        [reason, low, 0, 0, 0, 0, 0, 0, 0, KEY_VECTOR, 0, 0, 0],
    )
    .expect("called");
}

/// `low` pressed and released.
fn tap(s: &mut Session, low: u32) {
    press(s, low, true);
    press(s, low, false);
}

/// What the keyboard buffer holds, emptied.
fn typed(s: &mut Session) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let regs = swi(s, BYTE, [145, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("read");
        if regs.flag(psr::C) {
            return bytes;
        }
        bytes.push(regs.byte(2));
    }
}

/// `OS_Byte` `number` with R1 and R2: its R1 and R2.
fn byte(s: &mut Session, number: u32, r1: u32, r2: u32) -> (u32, u32) {
    let regs = swi(s, BYTE, [number, r1, r2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("called");
    (regs.r[1], regs.r[2])
}

/// The keyboard status made `value`.
fn set_status(s: &mut Session, value: u8) {
    byte(s, 202, u32::from(value), 0);
}

/// The clock moved on `centiseconds` and the guest run, so its interrupts
/// are taken.
fn wait(s: &mut Session, centiseconds: u64) {
    s.clock.set(s.clock.get() + centiseconds);
    let _ = s.run(10_000);
}

#[test]
fn a_handler_is_started_and_its_simple_keys_give_their_codes() {
    let mut s = session();
    let previous = install(&mut s);
    assert_ne!(previous, u32::MAX, "InternationalKeyboard's, replaced");
    assert_eq!(
        byte(&mut s, 202, 0, 0xFF).0.to_le_bytes()[0],
        STARTED,
        "its initialisation's status taken"
    );
    let _ = typed(&mut s);
    tap(&mut s, SIMPLE);
    set_status(&mut s, STARTED & !0x10);
    tap(&mut s, SIMPLE);
    set_status(&mut s, 0xA0);
    tap(&mut s, SIMPLE);
    press(&mut s, SHIFT, true);
    set_status(&mut s, 0xA8);
    tap(&mut s, SIMPLE);
    press(&mut s, SHIFT, false);
    assert_eq!(
        typed(&mut s),
        b"xXXx",
        "plain; Caps Lock; Shift-Caps, and with Shift held"
    );
    s.kernel.os().world.keyboard.shifts = 0x40;
    set_status(&mut s, STARTED | 0x40);
    tap(&mut s, SIMPLE);
    assert_eq!(typed(&mut s), [0x18], "Ctrl's column");
}

#[test]
fn the_next_key_puts_shift_and_ctrl_back_where_their_keys_are() {
    let mut s = session();
    install(&mut s);
    let _ = typed(&mut s);
    set_status(&mut s, STARTED | 0x08 | 0x40);
    tap(&mut s, SIMPLE);
    assert_eq!(typed(&mut s), b"x", "neither Shift nor Ctrl is held");
    assert_eq!(byte(&mut s, 202, 0, 0xFF).0.to_le_bytes()[0] & 0x48, 0);
}

#[test]
fn special_and_shifting_keys_call_the_handler_s_code() {
    let mut s = session();
    install(&mut s);
    let _ = typed(&mut s);
    tap(&mut s, SPECIAL);
    assert_eq!(typed(&mut s), b"pq", "its list");
    press(&mut s, SHIFT, true);
    assert_eq!(
        byte(&mut s, 202, 0, 0xFF).0,
        u32::from(SHIFT_DOWN),
        "the status it gave going down"
    );
    tap(&mut s, SIMPLE);
    press(&mut s, SHIFT, false);
    assert_eq!(
        byte(&mut s, 202, 0, 0xFF).0,
        u32::from(SHIFT_UP),
        "and going up"
    );
    tap(&mut s, SIMPLE);
    assert_eq!(typed(&mut s), b"Xx");
    set_status(&mut s, STARTED | 1);
    tap(&mut s, SIMPLE);
    assert_eq!(typed(&mut s), b"!", "a pending Alt: its code for that");
}

#[test]
fn the_current_key_repeats_after_the_delay_at_the_rate() {
    let mut s = session();
    install(&mut s);
    byte(&mut s, 11, 30, 0);
    byte(&mut s, 12, 10, 0);
    let _ = typed(&mut s);
    press(&mut s, SIMPLE, true);
    assert_eq!(
        typed(&mut s),
        b"x",
        "at once from a keyboard that debounces itself"
    );
    wait(&mut s, 29);
    assert!(typed(&mut s).is_empty());
    wait(&mut s, 1);
    assert_eq!(typed(&mut s), b"x", "after the delay");
    wait(&mut s, 10);
    assert_eq!(typed(&mut s), b"x", "then at the rate");
    press(&mut s, SIMPLE, false);
    wait(&mut s, 100);
    assert!(typed(&mut s).is_empty(), "released, it stops");
    byte(&mut s, 11, 0, 0);
    press(&mut s, SIMPLE, true);
    wait(&mut s, 100);
    press(&mut s, SIMPLE, false);
    assert_eq!(typed(&mut s), b"x", "no delay, no repeat");
}

#[test]
fn a_second_key_rolls_over_and_the_first_does_not_resume() {
    let mut s = session();
    install(&mut s);
    byte(&mut s, 11, 30, 0);
    let _ = typed(&mut s);
    press(&mut s, SIMPLE, true);
    press(&mut s, SPECIAL, true);
    assert_eq!(typed(&mut s), b"xpq");
    wait(&mut s, 30);
    assert_eq!(typed(&mut s), b"pq", "the second repeats");
    press(&mut s, SPECIAL, false);
    wait(&mut s, 100);
    assert!(
        typed(&mut s).is_empty(),
        "the first, still down, stays quiet"
    );
    press(&mut s, SIMPLE, false);
    press(&mut s, SIMPLE, false);
    press(&mut s, SIMPLE, true);
    press(&mut s, SIMPLE, true);
    assert_eq!(typed(&mut s), b"x", "a key already down is ignored");
}

#[test]
fn keys_are_read_by_internal_number_through_the_handler_s_table() {
    let mut s = session();
    install(&mut s);
    let inkey = 0xFF - SIMPLE_INTERNAL;
    assert_eq!(byte(&mut s, 129, inkey, 0xFF).0, 0, "up");
    press(&mut s, SIMPLE, true);
    assert_eq!(byte(&mut s, 129, inkey, 0xFF).0, 0xFF, "down");
    assert_eq!(
        byte(&mut s, 121, 0, 0),
        (SIMPLE_INTERNAL, 0),
        "the first found from nought"
    );
    assert_eq!(
        byte(&mut s, 121, SIMPLE_INTERNAL + 1, 0),
        (0xFF, 0x2000_0000),
        "none past it"
    );
    press(&mut s, SIMPLE, false);
}

#[test]
fn a_keyboard_s_arrival_restarts_the_handler_and_leaves_it_installed() {
    let mut s = session();
    install(&mut s);
    set_status(&mut s, 0);
    swi(
        &mut s,
        CALL_A_VECTOR,
        [0, 2, 0, 0, 0, 0, 0, 0, 0, KEY_VECTOR, 0, 0, 0],
    )
    .expect("arrived");
    assert_eq!(
        byte(&mut s, 202, 0, 0xFF).0.to_le_bytes()[0],
        STARTED,
        "the same keyboard: the handler started again"
    );
    swi(
        &mut s,
        CALL_A_VECTOR,
        [0, 1, 0, 0, 0, 0, 0, 0, 0, KEY_VECTOR, 0, 0, 0],
    )
    .expect("arrived");
    let read = swi(
        &mut s,
        INSTALL_KEY_HANDLER,
        [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("read");
    assert_eq!(read.r[0], 1, "the keyboard's identity");
    let now = swi(&mut s, INSTALL_KEY_HANDLER, [0; 13]).expect("read").r[0];
    assert_eq!(now, HANDLER, "a new keyboard: the program's handler stays");
}

#[test]
fn keyv_3_lights_scroll_num_and_caps_in_bits_0_1_and_2() {
    let mut s = session();
    let code = DATA + 0x2000;
    s.poke(
        code,
        &[
            asm::cmp_imm(0, 3),
            asm::when(Condition::Eq, asm::str(1, 12, 0)),
            asm::mov_pc_lr(),
        ],
    );
    swi(
        &mut s,
        0x1F,
        [
            KEY_VECTOR,
            code,
            DATA + 0x2100,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ],
    )
    .expect("claimed");
    // Scroll Lock on, Num Lock off, Caps Lock on.
    set_status(&mut s, 0b0000_0110);
    byte(&mut s, 118, 0, 0);
    assert_eq!(s.peek(DATA + 0x2100), 0b101);
}

#[test]
fn keys_are_ignored_until_keyv_4_enables_the_drivers() {
    let mut s = session();
    install(&mut s);
    typed(&mut s);
    s.kernel.os().world.keyboard.enabled = false;
    tap(&mut s, SIMPLE);
    assert!(typed(&mut s).is_empty(), "before the drivers are enabled");
    install(&mut s);
    tap(&mut s, SIMPLE);
    assert_eq!(typed(&mut s), b"x");
}

#[test]
fn os_byte_4_makes_copy_and_the_cursor_keys_edit_read_or_act_as_function_keys() {
    let mut s = session();
    let read = |s: &mut Session, code: u32| {
        byte(s, 138, 0, code);
        byte(s, 129, 0, 0)
    };
    assert_eq!(read(&mut s, 0x8C).1, 0xFF, "the copy cursor's, at first");
    byte(&mut s, 4, 1, 0);
    for (code, read_as) in (0x8B..=0x8F).zip(135..=139) {
        assert_eq!(read(&mut s, code), (read_as, 0), "{code:#x}");
    }
    assert_eq!(
        read(&mut s, 0x87).1,
        0xFF,
        "F7 is no cursor key: its Key$7 is empty"
    );
    byte(&mut s, 4, 2, 0);
    crate::cli::tests::cli(&mut s, "Key 12 k").expect("defined");
    assert_eq!(
        read(&mut s, 0x8C),
        (u32::from(b'k'), 0),
        "left arrow as F12"
    );
}

#[test]
fn os_byte_122_scans_from_internal_key_16() {
    let mut s = session();
    install(&mut s);
    // A key of the second extension range reads as internal key 15.
    press(&mut s, 0x200, true);
    assert_eq!(
        byte(&mut s, 121, 15, 0).0 & 0xFF,
        15,
        "OS_Byte 121 from 15 finds it"
    );
    assert_eq!(byte(&mut s, 122, 0, 0).0, 0xFF, "122 starts past it");
    press(&mut s, 0x200, false);
}

#[test]
fn a_nul_in_the_buffer_keeps_the_next_byte_as_itself() {
    let mut s = session();
    let read = |s: &mut Session| byte(s, 129, 0, 0);
    let insert = |s: &mut Session, bytes: &[u32]| {
        for &b in bytes {
            byte(s, 138, 0, b);
        }
    };
    crate::cli::tests::cli(&mut s, "Key 9 nine").expect("defined");
    insert(&mut s, &[0, 0xC9]);
    assert_eq!(read(&mut s), (0xC9, 0), "a character, not F9");
    insert(&mut s, &[0, 0]);
    assert_eq!(read(&mut s), (0, 0), "Ctrl-@");
    assert_eq!(
        read(&mut s).1,
        0xFF,
        "once, while no block reads as NUL and code"
    );
    byte(&mut s, 221, 2, 0);
    insert(&mut s, &[0, 0]);
    assert_eq!(read(&mut s), (0, 0));
    assert_eq!(read(&mut s), (0, 0), "twice, once one does");
}

#[test]
fn insv_and_remv_move_blocks_as_far_as_the_buffer_allows() {
    use crate::input::BLOCK;
    let mut s = session();
    let _ = typed(&mut s);
    let data = DATA + 0x2000;
    s.write(data, b"abc");
    let inserted = swi(
        &mut s,
        CALL_A_VECTOR,
        [0, BLOCK, data, 3, 0, 0, 0, 0, 0, vectors::INS.0, 0, 0, 0],
    )
    .expect("called");
    assert_eq!((inserted.r[2], inserted.r[3]), (data + 3, 0));
    assert!(!inserted.flag(psr::C), "all three went in");
    let out = DATA + 0x2100;
    let removed = swi(
        &mut s,
        CALL_A_VECTOR,
        [0, BLOCK, out, 2, 0, 0, 0, 0, 0, vectors::REM.0, 0, 0, 0],
    )
    .expect("called");
    assert_eq!((removed.r[2], removed.r[3]), (out + 2, 0));
    assert_eq!(s.read(out, 2), b"ab");
    assert_eq!(typed(&mut s), b"c", "the rest left in the buffer");
    let empty = swi(
        &mut s,
        CALL_A_VECTOR,
        [0, BLOCK, out, 4, 0, 0, 0, 0, 0, vectors::REM.0, 0, 0, 0],
    )
    .expect("called");
    assert!(empty.flag(psr::C), "empty");
    assert_eq!(empty.r[3], 4, "nothing came");
    let many = alloc::vec![b'z'; 300];
    s.write(data, &many);
    let filled = swi(
        &mut s,
        CALL_A_VECTOR,
        [0, BLOCK, data, 300, 0, 0, 0, 0, 0, vectors::INS.0, 0, 0, 0],
    )
    .expect("called");
    assert!(filled.flag(psr::C), "the keyboard buffer holds fewer");
    assert_eq!(
        filled.r[2] - data + filled.r[3],
        300,
        "R2 and R3 agree on what is left"
    );
    let _ = typed(&mut s);
}

#[test]
fn shift_enable_reverses_shift_only_under_caps_lock() {
    let mut s = session();
    install(&mut s);
    let _ = typed(&mut s);
    // Shift Enable on, Caps Lock off.
    set_status(&mut s, 0xB0);
    tap(&mut s, SIMPLE);
    press(&mut s, SHIFT, true);
    set_status(&mut s, 0xB8);
    tap(&mut s, SIMPLE);
    press(&mut s, SHIFT, false);
    assert_eq!(typed(&mut s), b"xX", "as with Shift Enable off");
}

#[test]
fn the_next_key_shows_the_locks_os_byte_202_wrote() {
    let mut s = session();
    install(&mut s);
    let code = DATA + 0x2000;
    s.poke(
        code,
        &[
            asm::cmp_imm(0, 3),
            asm::when(Condition::Eq, asm::str(1, 12, 0)),
            asm::mov_pc_lr(),
        ],
    );
    let lit = DATA + 0x2100;
    swi(
        &mut s,
        0x1F,
        [KEY_VECTOR, code, lit, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("claimed");
    s.poke(lit, &[0xEE]);
    // Caps Lock on, through OS_Byte 202 alone.
    set_status(&mut s, STARTED & !0x10);
    assert_eq!(s.peek(lit), 0xEE, "OS_Byte 202 leaves the LEDs");
    press(&mut s, SIMPLE, true);
    assert_eq!(
        s.peek(lit),
        0b110,
        "the key going down lights Caps and Num Lock, as the status has them"
    );
    press(&mut s, SIMPLE, false);
}
