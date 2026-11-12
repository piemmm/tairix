//! The keys: the keypad's digits told from its moves, and the letters of
//! Ctrl and Alt combinations.

use super::*;

const SHIFT: u8 = shift::LEFT_SHIFT;

#[test]
fn a_keypad_digit_with_shift_held_is_its_move_selecting() {
    let left = Press::of(0x4B34, SHIFT);
    assert_eq!(
        (left.char, left.scan, left.shift),
        (None, scan::LEFT, true),
        "Shift and keypad 4, NumLock off"
    );
    let digit = Press::of(0x4B34, shift::NUM);
    assert_eq!(
        (digit.char, digit.shift),
        (Some(b'4'), false),
        "NumLock's digit"
    );
    let unshifted = Press::of(0x4B00, shift::NUM | SHIFT);
    assert_eq!(
        (unshifted.char, unshifted.shift),
        (None, false),
        "Shift undoing NumLock gives the plain move"
    );
    let cut = Press::of(0x532E, SHIFT);
    assert!(cut.is(scan::DELETE) && cut.shift, "Shift+Del");
    let plain = Press::of(0x4800, 0);
    assert!(plain.is(scan::UP) && !plain.shift);
    let minus = Press::of(0x4A2D, SHIFT);
    assert_eq!(minus.char, Some(b'-'), "the grey minus stays a character");
}

#[test]
fn a_combinations_letter_is_found_from_its_character_or_its_key() {
    assert_eq!(
        Press::of(0x1011, shift::CTRL).letter(),
        Some(b'Q'),
        "Ctrl+Q"
    );
    assert_eq!(Press::of(0x2100, shift::ALT).letter(), Some(b'F'), "Alt+F");
    assert_eq!(
        Press::of(0x0E08, 0).letter(),
        None,
        "Backspace is no Ctrl+H"
    );
    assert_eq!(Press::of(0x2308, shift::CTRL).letter(), Some(b'H'));
    assert_eq!(Press::of(0x1C0A, shift::CTRL).letter(), None, "Ctrl+Enter");
    assert_eq!(Press::of(0x0B30, 0).letter(), Some(b'0'));
    assert_eq!(
        (
            letter_of(0x10),
            letter_of(0x26),
            letter_of(0x32),
            letter_of(0x27)
        ),
        (Some(b'Q'), Some(b'L'), Some(b'M'), None)
    );
}
