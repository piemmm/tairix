//! InternationalKeyboard's tests: the handler it installs and its tables,
//! the layouts' characters through Shift, Ctrl, Alt and Caps Lock, dead
//! keys, Alt entries by number, the keypad, layers, the hot keys, and what
//! it leaves when it dies.

use alloc::vec::Vec;

use tairix_riscos_arm::psr;

use super::{
    key, CODES_AT, DECIMAL, INKEY_AT, KEYS, SHIFTING, SHIFTING_AT, SPECIAL_AT, TRANSLATION_AT,
};
use crate::cli::tests::{cli, session};
use crate::keyboard::status;
use crate::msgtrans::tests::swi;
use crate::swi::os::{BYTE, CALL_A_VECTOR, INSTALL_KEY_HANDLER};
use crate::testkit::Session;

/// Keys the tests press.
const A: u32 = 0x3C;
const B: u32 = 0x52;
const D: u32 = 0x3E;
const E: u32 = 0x29;
const G: u32 = 0x40;
const Y: u32 = 0x2C;
const TWO: u32 = 0x12;
const FOUR: u32 = 0x14;
const SEMICOLON: u32 = 0x45;
const QUOTE: u32 = 0x46;
const ESCAPE: u32 = 0x00;
const DOTLESS_I: u32 = 0x2E;
const KEYPAD_0: u32 = 0x65;
const KEYPAD_2: u32 = 0x5B;
const KEYPAD_3: u32 = 0x5C;
const KEYPAD_4: u32 = 0x48;
const KEYPAD_5: u32 = 0x49;
const KEYPAD_7: u32 = 0x37;
const KEYPAD_9: u32 = 0x39;
const KEYPAD_PLUS: u32 = 0x4B;

/// A low-level key as its tables hold it.
fn low(key: u32) -> u8 {
    u8::try_from(key).expect("a key below &100")
}

/// Low-level key `low` going down or up, through `KeyV`.
fn press(s: &mut Session, low: u32, down: bool) {
    let reason = if down { 2 } else { 1 };
    swi(
        s,
        CALL_A_VECTOR,
        [
            reason,
            low,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            crate::vectors::KEY.0,
            0,
            0,
            0,
        ],
    )
    .expect("called");
}

/// `low` pressed and released with `held` held.
fn tap(s: &mut Session, held: &[u32], low: u32) {
    for &key in held {
        press(s, key, true);
    }
    press(s, low, true);
    press(s, low, false);
    for &key in held.iter().rev() {
        press(s, key, false);
    }
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

/// `OS_Byte` `number` with R1 and R2: its R1.
fn byte(s: &mut Session, number: u32, r1: u32, r2: u32) -> u32 {
    swi(s, BYTE, [number, r1, r2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        .expect("called")
        .r[1]
}

/// The keyboard status, `OS_Byte` 202's.
fn keyboard_status(s: &mut Session) -> u8 {
    u8::try_from(byte(s, 202, 0, 0xFF)).expect("a byte")
}

/// The keyboard status made `value`.
fn set_status(s: &mut Session, value: u8) {
    byte(s, 202, u32::from(value), 0);
}

/// The installed handler's address.
fn handler(s: &mut Session) -> u32 {
    swi(s, INSTALL_KEY_HANDLER, [0; 13]).expect("read").r[0]
}

/// A session with Caps Lock off, Num Lock on, and `keyboard` selected in
/// `alphabet`.
fn keyboard_in(keyboard: &str, alphabet: &str) -> Session {
    let mut s = session();
    cli(&mut s, &alloc::format!("Keyboard {keyboard}")).expect("selected");
    cli(&mut s, &alloc::format!("Alphabet {alphabet}")).expect("selected");
    let state = keyboard_status(&mut s);
    set_status(
        &mut s,
        (state | status::NO_CAPS_LOCK) & !(status::NO_NUM_LOCK | status::SHIFT_CAPS),
    );
    let _ = typed(&mut s);
    s
}

#[test]
fn its_handler_is_installed_with_its_header_and_tables() {
    let mut s = session();
    let at = handler(&mut s);
    assert_ne!(at, u32::MAX);
    assert_eq!(s.peek(at), TRANSLATION_AT);
    assert_eq!(s.peek(at + 4), KEYS, "narrow, no flags word");
    assert_eq!(
        s.peek(at + TRANSLATION_AT + 4 * ESCAPE),
        0x1B1B_1B1B,
        "Escape is the kernel's to make"
    );
    assert_eq!(
        s.peek(at + TRANSLATION_AT + 4 * A),
        u32::MAX,
        "a has an Alt level: special"
    );
    let d = u32::from_le_bytes([b'd', b'D', 4, 4]);
    assert_eq!(s.peek(at + TRANSLATION_AT + 4 * D), d);
    assert_eq!(
        s.peek(at + INKEY_AT + 4 * 127),
        0xFFFF_4C58,
        "internal key 0: either Shift"
    );
    assert_eq!(s.read(at + SHIFTING_AT, 1)[0] as usize, SHIFTING.len());
    let count = u32::from(s.read(at + SPECIAL_AT, 1)[0]);
    let specials = s.read(at + SPECIAL_AT + 1, count as usize);
    assert!(specials.contains(&low(key::LEFT_SHIFT)) && specials.contains(&low(A)));
    assert!(!specials.contains(&low(D)), "a simple key is not special");
    let code = at + CODES_AT;
    let first = code.wrapping_add(s.peek(code));
    assert!(
        (0..count).all(|i| code.wrapping_add(s.peek(code + 4 * i)) == first),
        "one routine for them all"
    );
    let checked = swi(&mut s, 0x58, [13, at, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("valid");
    assert_eq!(checked.r[0], 1);
}

#[test]
fn letters_follow_shift_and_caps_lock() {
    let mut s = keyboard_in("UK", "Latin1");
    tap(&mut s, &[], A);
    tap(&mut s, &[key::LEFT_SHIFT], A);
    tap(&mut s, &[], D);
    tap(&mut s, &[key::RIGHT_SHIFT], D);
    assert_eq!(typed(&mut s), b"aAdD");
    tap(&mut s, &[], key::CAPS_LOCK);
    tap(&mut s, &[], A);
    tap(&mut s, &[key::LEFT_SHIFT], A);
    tap(&mut s, &[], D);
    assert_eq!(
        typed(&mut s),
        b"AAD",
        "Caps Lock gives capitals, Shift or not"
    );
    tap(&mut s, &[], key::CAPS_LOCK);
    tap(&mut s, &[key::LEFT_SHIFT], key::CAPS_LOCK);
    assert_ne!(
        keyboard_status(&mut s) & status::SHIFT_CAPS,
        0,
        "Shift with Caps Lock: Shift-Caps"
    );
    tap(&mut s, &[], A);
    tap(&mut s, &[key::LEFT_SHIFT], A);
    tap(&mut s, &[key::LEFT_SHIFT], D);
    assert_eq!(typed(&mut s), b"Aad", "Shift reverses it");
}

#[test]
fn ctrl_gives_each_key_s_control_code() {
    let mut s = keyboard_in("UK", "Latin1");
    tap(&mut s, &[key::LEFT_CTRL], A);
    tap(&mut s, &[key::LEFT_CTRL], D);
    tap(&mut s, &[key::LEFT_CTRL], TWO);
    tap(&mut s, &[key::LEFT_CTRL], key::SPACE);
    assert_eq!(typed(&mut s), [1, 4, 2, b' ']);
    tap(&mut s, &[key::LEFT_CTRL], QUOTE);
    assert_eq!(typed(&mut s), [0, 0], "Ctrl with the @ key: a NUL");
    let mut s = keyboard_in("Greece", "Greek");
    tap(&mut s, &[key::LEFT_CTRL], E);
    assert_eq!(typed(&mut s), [5], "ε's key reads as E's");
}

#[test]
fn alt_gives_the_layout_s_alt_level_in_the_alphabet() {
    let mut s = keyboard_in("UK", "Latin1");
    tap(&mut s, &[key::RIGHT_ALT], FOUR);
    tap(&mut s, &[key::RIGHT_ALT], E);
    tap(&mut s, &[key::RIGHT_ALT, key::LEFT_SHIFT], E);
    tap(&mut s, &[key::RIGHT_ALT], key::SPACE);
    assert_eq!(
        typed(&mut s),
        [0, 0x80, 0, 0xE9, 0, 0xC9, 0, 0xA0],
        "RISC OS Latin1's euro, é, É, a hard space"
    );
    tap(&mut s, &[key::RIGHT_ALT], D);
    assert!(typed(&mut s).is_empty(), "d has no Alt level");
    let mut s = keyboard_in("UK", "UTF8");
    tap(&mut s, &[key::RIGHT_ALT], FOUR);
    assert_eq!(
        typed(&mut s),
        [0, 0xE2, 0, 0x82, 0, 0xAC],
        "the euro in UTF-8"
    );
}

#[test]
fn a_dead_key_s_accent_composes_with_the_next_character() {
    let mut s = keyboard_in("UK", "Latin1");
    tap(&mut s, &[key::RIGHT_ALT], QUOTE);
    assert!(typed(&mut s).is_empty(), "a dead key gives nothing");
    assert_ne!(
        keyboard_status(&mut s) & status::PENDING_ALT,
        0,
        "the accent waits"
    );
    tap(&mut s, &[], E);
    assert_eq!(typed(&mut s), [0, 0xE9]);
    assert_eq!(keyboard_status(&mut s) & status::PENDING_ALT, 0);
    tap(&mut s, &[key::RIGHT_ALT], QUOTE);
    tap(&mut s, &[], key::SPACE);
    assert_eq!(typed(&mut s), [0, 0xB4], "the accent alone");
    tap(&mut s, &[key::RIGHT_ALT], QUOTE);
    tap(&mut s, &[], D);
    assert_eq!(
        typed(&mut s),
        b"d",
        "a simple key it does not compose with, the accent used up"
    );
    tap(&mut s, &[key::RIGHT_ALT], QUOTE);
    tap(&mut s, &[], G);
    assert_eq!(
        typed(&mut s),
        b"g",
        "ǵ has no code in Latin1: the letter alone"
    );
    let mut s = keyboard_in("UK", "UTF8");
    tap(&mut s, &[key::RIGHT_ALT], QUOTE);
    tap(&mut s, &[], G);
    assert_eq!(typed(&mut s), [0, 0xC7, 0, 0xB5], "but it has in UTF-8");
}

#[test]
fn alt_and_the_keypad_type_a_character_by_its_number() {
    let mut s = keyboard_in("UK", "Latin1");
    press(&mut s, key::LEFT_ALT, true);
    for digit in [KEYPAD_2, KEYPAD_3, KEYPAD_3] {
        tap(&mut s, &[], digit);
    }
    assert!(typed(&mut s).is_empty(), "nothing until Alt goes up");
    press(&mut s, key::LEFT_ALT, false);
    assert_eq!(typed(&mut s), [0, 233]);
    press(&mut s, key::LEFT_ALT, true);
    for digit in [KEYPAD_0, KEYPAD_2, KEYPAD_3, KEYPAD_3] {
        tap(&mut s, &[], digit);
    }
    tap(&mut s, &[], B);
    press(&mut s, key::LEFT_ALT, false);
    assert_eq!(
        typed(&mut s),
        [0, 233],
        "decimal whatever leads it, a letter key giving nothing"
    );
    press(&mut s, key::LEFT_ALT, true);
    tap(&mut s, &[], KEYPAD_4);
    tap(&mut s, &[], KEYPAD_PLUS);
    press(&mut s, key::LEFT_ALT, false);
    assert!(typed(&mut s).is_empty(), "a key no digit abandons it");
}

#[test]
fn the_keypad_follows_num_lock_and_the_layout_s_decimal() {
    let mut s = keyboard_in("UK", "Latin1");
    tap(&mut s, &[], KEYPAD_7);
    tap(&mut s, &[], DECIMAL);
    assert_eq!(typed(&mut s), b"7.");
    tap(&mut s, &[], key::NUM_LOCK);
    tap(&mut s, &[], KEYPAD_7);
    tap(&mut s, &[], KEYPAD_5);
    tap(&mut s, &[], DECIMAL);
    assert_eq!(typed(&mut s), [0x1E, 0x7F], "Home, nothing, Delete");
    let mut s = keyboard_in("Germany", "Latin1");
    tap(&mut s, &[], DECIMAL);
    assert_eq!(typed(&mut s), b",", "a German keypad's decimal comma");
}

#[test]
fn a_keyboard_s_layout_takes_effect_when_it_is_selected() {
    let mut s = keyboard_in("Germany", "Latin1");
    tap(&mut s, &[], Y);
    tap(&mut s, &[], SEMICOLON);
    assert_eq!(typed(&mut s), [b'z', 0, 0xF6], "QWERTZ, and ö");
    let at = handler(&mut s);
    assert_eq!(
        s.read(at + TRANSLATION_AT + 4 * Y, 2),
        b"zZ",
        "its tables rewritten"
    );
    cli(&mut s, "Keyboard UK").expect("selected");
    tap(&mut s, &[], Y);
    assert_eq!(typed(&mut s), b"y");
    assert_eq!(handler(&mut s), at, "the same handler, not installed again");
}

#[test]
fn a_second_layer_is_switched_to_by_kana_or_left_alt_with_shift() {
    let mut s = keyboard_in("Russia", "UTF8");
    tap(&mut s, &[], A);
    tap(&mut s, &[key::LEFT_ALT], key::LEFT_SHIFT);
    tap(&mut s, &[], A);
    assert_eq!(typed(&mut s), [b'a', 0, 0xD1, 0, 0x84], "a, then ф");
    tap(&mut s, &[key::LEFT_SHIFT], key::LEFT_ALT);
    tap(&mut s, &[], A);
    assert_eq!(typed(&mut s), b"a", "and back, either way about");
    let mut s = keyboard_in("Japan", "UTF8");
    tap(&mut s, &[], key::KANA);
    tap(&mut s, &[], 0x11);
    assert_eq!(typed(&mut s), [0, 0xE3, 0, 0x81, 0, 0xAC], "ぬ");
}

#[test]
fn turkish_capitals_keep_their_dots() {
    let mut s = keyboard_in("Turkey", "UTF8");
    tap(&mut s, &[], key::CAPS_LOCK);
    tap(&mut s, &[], QUOTE);
    tap(&mut s, &[], DOTLESS_I);
    assert_eq!(typed(&mut s), [0, 0xC4, 0, 0xB0, b'I'], "İ and I");
}

#[test]
fn hot_keys_select_the_uk_keyboard_and_a_dialled_country_s() {
    let mut s = keyboard_in("Germany", "Latin1");
    tap(&mut s, &[key::LEFT_ALT, key::LEFT_CTRL], key::F1);
    assert_eq!(byte(&mut s, 71, 0xFF, 0), 1, "the UK's, by a callback");
    press(&mut s, key::LEFT_ALT, true);
    tap(&mut s, &[key::LEFT_CTRL], key::F12);
    tap(&mut s, &[], KEYPAD_4);
    tap(&mut s, &[], KEYPAD_9);
    press(&mut s, key::LEFT_ALT, false);
    assert!(typed(&mut s).is_empty(), "a dialling code types nothing");
    assert_eq!(byte(&mut s, 71, 0xFF, 0), 7, "+49, Germany's");
}

#[test]
fn keys_read_by_internal_number_through_its_table() {
    let mut s = keyboard_in("UK", "Latin1");
    press(&mut s, D, true);
    let regs = swi(
        &mut s,
        BYTE,
        [129, 0xCD, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("read");
    assert_eq!(regs.r[1], 0xFF, "D, internal key 50, is down");
    press(&mut s, D, false);
    let regs = swi(
        &mut s,
        BYTE,
        [129, 0xCD, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("read");
    assert_eq!(regs.r[1], 0);
    let _ = typed(&mut s);
}

#[test]
fn escape_and_the_mouse_buttons_take_the_kernel_s_exits() {
    let mut s = keyboard_in("UK", "Latin1");
    tap(&mut s, &[], ESCAPE);
    assert!(
        s.kernel.os().escape_set(),
        "the escape character raises escape"
    );
    let _ = byte(&mut s, 126, 0, 0);
    press(&mut s, key::SELECT, true);
    assert_eq!(s.kernel.os().world.input.mouse.buttons, 4, "Select held");
    press(&mut s, key::ADJUST, true);
    press(&mut s, key::SELECT, false);
    assert_eq!(s.kernel.os().world.input.mouse.buttons, 1, "Adjust held");
    press(&mut s, key::ADJUST, false);
    assert_eq!(s.kernel.os().world.input.mouse.buttons, 0);
}

#[test]
fn it_puts_the_previous_handler_back_when_it_dies() {
    let mut s = session();
    assert_ne!(handler(&mut s), u32::MAX);
    cli(&mut s, "RMKill InternationalKeyboard").expect("killed");
    assert_eq!(handler(&mut s), u32::MAX, "none was there before it");
    tap(&mut s, &[], D);
    assert!(typed(&mut s).is_empty());
    cli(&mut s, "RMReInit InternationalKeyboard").expect("restarted");
    tap(&mut s, &[], D);
    assert_eq!(typed(&mut s), b"d");
}

#[test]
fn an_accent_waiting_composes_with_a_key_typed_under_alt() {
    let mut s = keyboard_in("UK", "UTF8");
    tap(&mut s, &[key::RIGHT_ALT], QUOTE);
    press(&mut s, key::RIGHT_ALT, true);
    tap(&mut s, &[], G);
    press(&mut s, key::RIGHT_ALT, false);
    assert_eq!(typed(&mut s), [0, 0xC7, 0, 0xB5], "ǵ, though Alt is held");
    tap(&mut s, &[key::RIGHT_ALT], G);
    assert!(
        typed(&mut s).is_empty(),
        "with no accent, Alt and a simple key give nothing"
    );
}

#[test]
fn a_hot_key_that_could_not_be_scheduled_leaves_none_pending() {
    let mut s = keyboard_in("Germany", "Latin1");
    let workspace = s.kernel.os().world.intkey.workspace.take();
    s.kernel.os().select_later(0x81).expect("refused quietly");
    assert_eq!(
        s.kernel.os().world.intkey.selecting,
        None,
        "nothing waits on a callback never added"
    );
    s.kernel.os().world.intkey.workspace = workspace;
    tap(&mut s, &[key::LEFT_ALT, key::LEFT_CTRL], key::F1);
    assert_eq!(byte(&mut s, 71, 0xFF, 0), 1, "the next hot key still works");
}

#[test]
fn break_is_the_escape_key_or_a_reset_as_os_byte_247_says() {
    let mut s = keyboard_in("UK", "Latin1");
    byte(&mut s, 220, u32::from(b'q'), 0);
    press(&mut s, key::BREAK, true);
    assert!(
        s.kernel.os().escape_set(),
        "the escape character, whatever it is"
    );
    press(&mut s, key::BREAK, false);
    let _ = byte(&mut s, 126, 0, 0);
    byte(&mut s, 247, 0, 0);
    press(&mut s, key::LEFT_CTRL, true);
    press(&mut s, key::BREAK, true);
    let up = s.start_swi(
        0x2_0000 | CALL_A_VECTOR,
        [
            1,
            key::BREAK,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            crate::vectors::KEY.0,
            0,
            0,
            0,
        ],
    );
    assert_eq!(
        up,
        crate::os::Outcome::Ended(crate::os::Ended::Reset {
            hard: true,
            reverse_boot: false
        }),
        "Ctrl-Break, a hard reset"
    );
}

#[test]
fn only_a_country_s_own_dialling_code_selects_its_keyboard() {
    let mut s = keyboard_in("UK", "Latin1");
    press(&mut s, key::LEFT_ALT, true);
    tap(&mut s, &[key::LEFT_CTRL], key::F12);
    for digit in [KEYPAD_9, KEYPAD_9, KEYPAD_4, KEYPAD_4] {
        tap(&mut s, &[], digit);
    }
    press(&mut s, key::LEFT_ALT, false);
    assert_eq!(byte(&mut s, 71, 0xFF, 0), 1, "no country dials 9944");
}
