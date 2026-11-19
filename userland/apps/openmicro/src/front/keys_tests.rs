use alloc::vec::Vec;

use tairix_abi::input::Modifiers;

use super::*;
use crate::machine::bbc::keyboard::Key;
use crate::machine::electron::keyboard::Key as EKey;

type Keyboard = super::Keyboard<Key>;
type Press = super::Press<Key>;

const LEFT_SHIFT: u16 = 0xE1;
const RIGHT_SHIFT: u16 = 0xE5;

fn edge(usage: u16, key: Option<KeyValue>, pressed: bool) -> KeyInput {
    let physical = PhysicalKey::from_usage(usage).expect("a key");
    let modifiers = Modifiers::default();
    if pressed {
        KeyInput::Pressed {
            key,
            modifiers,
            physical,
        }
    } else {
        KeyInput::Released {
            key,
            modifiers,
            physical,
        }
    }
}

/// A machine's changes for each edge in turn: a usage, what it typed, and
/// whether it went down.
fn presses<K: MachineKey>(
    keyboard: &mut super::Keyboard<K>,
    edges: &[(u16, Option<KeyValue>, bool)],
) -> Vec<super::Press<K>> {
    let mut out = Vec::new();
    for &(usage, key, pressed) in edges {
        keyboard.edge(&edge(usage, key, pressed), &mut out);
    }
    out
}

const fn ch(c: char) -> KeyValue {
    KeyValue::Char(c)
}

const fn named(key: NamedKeyCode) -> KeyValue {
    KeyValue::Named(key)
}

#[test]
fn a_physical_key_is_the_bbc_key_in_its_place() {
    let mut keyboard = Keyboard::new(Mapping::Physical);
    assert_eq!(
        presses(
            &mut keyboard,
            &[(0x1D, Some(ch('z')), true), (0x1D, Some(ch('z')), false)]
        ),
        [Press::Key(Key::Z, true), Press::Key(Key::Z, false)]
    );
    for (usage, key) in [
        (0x34, Key::COLON),
        (0x38, Key::SLASH),
        (0x31, Key::RIGHT_BRACKET),
        (0x32, Key::RIGHT_BRACKET),
        (0x64, Key::BACKSLASH),
        (0x35, Key::UNDERSCORE),
        (0x27, Key::DIGIT_0),
        (0x1E, Key::DIGIT_1),
        (0x3A, Key::F1),
        (0x43, Key::F0),
        (0x4D, Key::COPY),
        (0x4A, Key::SHIFT_LOCK),
        (0x39, Key::CAPS_LOCK),
    ] {
        assert_eq!(
            presses(&mut keyboard, &[(usage, None, true), (usage, None, false)]),
            [Press::Key(key, true), Press::Key(key, false)],
            "{usage:#x}"
        );
    }
    assert_eq!(
        presses(&mut keyboard, &[(0x45, None, true), (0x45, None, false)]),
        [Press::Break(true), Press::Break(false)],
        "F12 is BREAK"
    );
    assert!(
        presses(&mut keyboard, &[(0xE3, None, true), (0xE3, None, false)]).is_empty(),
        "the BBC has no GUI key"
    );
}

#[test]
fn a_bbc_key_two_host_keys_hold_goes_up_with_the_last() {
    let mut keyboard = Keyboard::new(Mapping::Physical);
    assert_eq!(
        presses(
            &mut keyboard,
            &[
                (LEFT_SHIFT, None, true),
                (RIGHT_SHIFT, None, true),
                (LEFT_SHIFT, None, false),
            ]
        ),
        [Press::Key(Key::SHIFT, true)]
    );
    assert_eq!(
        presses(&mut keyboard, &[(RIGHT_SHIFT, None, false)]),
        [Press::Key(Key::SHIFT, false)]
    );
    assert_eq!(
        presses(
            &mut keyboard,
            &[(0x2A, None, true), (0x4C, None, true), (0x2A, None, false)]
        ),
        [Press::Key(Key::DELETE, true)],
        "Backspace and Delete are both DELETE"
    );
    assert_eq!(
        presses(&mut keyboard, &[(0x4C, None, false)]),
        [Press::Key(Key::DELETE, false)]
    );
}

#[test]
fn a_repeated_press_and_a_release_never_pressed_change_nothing() {
    let mut keyboard = Keyboard::new(Mapping::Physical);
    assert_eq!(
        presses(&mut keyboard, &[(0x04, None, true), (0x04, None, true)]),
        [Press::Key(Key::A, true)]
    );
    assert!(presses(&mut keyboard, &[(0x05, None, false)]).is_empty());
}

#[test]
fn a_symbol_presses_the_shift_it_needs_and_gives_the_hosts_back() {
    let mut keyboard = Keyboard::new(Mapping::Logical);
    assert_eq!(
        presses(
            &mut keyboard,
            &[
                (LEFT_SHIFT, None, true),
                (0x1F, Some(ch('@')), true),
                (0x1F, Some(ch('@')), false)
            ]
        ),
        [
            Press::Key(Key::SHIFT, true),
            Press::Key(Key::SHIFT, false),
            Press::Key(Key::AT, true),
            Press::Key(Key::AT, false),
            Press::Key(Key::SHIFT, true),
        ],
        "Shift-2 types @, which the BBC types unshifted"
    );
    assert_eq!(
        presses(
            &mut keyboard,
            &[(0x34, Some(ch('"')), true), (0x34, Some(ch('"')), false)]
        ),
        [
            Press::Key(Key::DIGIT_2, true),
            Press::Key(Key::DIGIT_2, false)
        ],
        "the BBC's \" is SHIFT-2, the host's Shift already down"
    );
    assert_eq!(
        presses(&mut keyboard, &[(LEFT_SHIFT, None, false)]),
        [Press::Key(Key::SHIFT, false)]
    );
    assert_eq!(
        presses(
            &mut keyboard,
            &[(0x2E, Some(ch('=')), true), (0x2E, Some(ch('=')), false)]
        ),
        [
            Press::Key(Key::SHIFT, true),
            Press::Key(Key::MINUS, true),
            Press::Key(Key::MINUS, false),
            Press::Key(Key::SHIFT, false),
        ],
        "the BBC's = is SHIFT and -"
    );
}

#[test]
fn a_letter_takes_its_case_from_the_bbc_and_the_hosts_shift() {
    let mut keyboard = Keyboard::new(Mapping::Logical);
    assert_eq!(
        presses(
            &mut keyboard,
            &[(0x04, Some(ch('a')), true), (0x04, Some(ch('a')), false)]
        ),
        [Press::Key(Key::A, true), Press::Key(Key::A, false)]
    );
    assert_eq!(
        presses(
            &mut keyboard,
            &[
                (LEFT_SHIFT, None, true),
                (0x04, Some(ch('A')), true),
                (0x04, Some(ch('A')), false),
                (LEFT_SHIFT, None, false)
            ]
        ),
        [
            Press::Key(Key::SHIFT, true),
            Press::Key(Key::A, true),
            Press::Key(Key::A, false),
            Press::Key(Key::SHIFT, false),
        ]
    );
}

#[test]
fn named_keys_and_locks_type_the_bbcs_own() {
    let mut keyboard = Keyboard::new(Mapping::Logical);
    for (usage, key, bbc) in [
        (0x28, NamedKeyCode::Enter, Key::RETURN),
        (0x2A, NamedKeyCode::Backspace, Key::DELETE),
        (0x4D, NamedKeyCode::End, Key::COPY),
        (0x52, NamedKeyCode::Up, Key::UP),
        (0x3A, NamedKeyCode::F1, Key::F1),
        (0x42, NamedKeyCode::F9, Key::F9),
        (0x43, NamedKeyCode::F10, Key::F0),
    ] {
        assert_eq!(
            presses(
                &mut keyboard,
                &[
                    (usage, Some(named(key)), true),
                    (usage, Some(named(key)), false)
                ]
            ),
            [Press::Key(bbc, true), Press::Key(bbc, false)],
            "{key:?}"
        );
    }
    assert_eq!(
        presses(
            &mut keyboard,
            &[
                (0x45, Some(named(NamedKeyCode::F12)), true),
                (0x45, Some(named(NamedKeyCode::F12)), false)
            ]
        ),
        [Press::Break(true), Press::Break(false)]
    );
    assert_eq!(
        presses(&mut keyboard, &[(0x39, None, true), (0x39, None, false)]),
        [
            Press::Key(Key::CAPS_LOCK, true),
            Press::Key(Key::CAPS_LOCK, false)
        ]
    );
    assert!(
        presses(&mut keyboard, &[(0x08, Some(ch('é')), true)]).is_empty(),
        "no BBC key types é"
    );
}

#[test]
fn losing_the_focus_or_the_mapping_lets_every_key_go() {
    let mut keyboard = Keyboard::new(Mapping::Logical);
    presses(
        &mut keyboard,
        &[
            (LEFT_SHIFT, None, true),
            (0x1F, Some(ch('@')), true),
            (0x45, Some(named(NamedKeyCode::F12)), true),
        ],
    );
    let mut out = Vec::new();
    keyboard.set_mapping(Mapping::Physical, &mut out);
    assert_eq!(
        out,
        [Press::Key(Key::AT, false), Press::Break(false)],
        "SHIFT, which @ had lifted, stays up"
    );
    assert_eq!(keyboard.mapping(), Mapping::Physical);
    out.clear();
    keyboard.release_all(&mut out);
    assert!(out.is_empty(), "nothing held");
}

#[test]
fn pasted_text_takes_shift_from_its_case_and_the_machines() {
    assert_eq!(
        typing::<Key>('a', LetterCase::Capitals),
        Some((Key::A, true)),
        "CAPS LOCK on, SHIFT for lower case"
    );
    assert_eq!(
        typing::<Key>('A', LetterCase::Capitals),
        Some((Key::A, false))
    );
    assert_eq!(typing::<Key>('a', LetterCase::Small), Some((Key::A, false)));
    assert_eq!(typing::<Key>('A', LetterCase::Small), Some((Key::A, true)));
    let letter = Keycap::Matrix(C64Key::A);
    assert_eq!(
        typing::<Keycap>('A', LetterCase::CapitalsOnly),
        Some((letter, false)),
        "the C64's upper-case set, where SHIFT makes a graphic"
    );
    assert_eq!(
        typing::<Keycap>('a', LetterCase::CapitalsOnly),
        Some((letter, false)),
        "a small letter typed as its capital"
    );
    assert_eq!(
        typing::<Keycap>('A', LetterCase::Small),
        Some((letter, true)),
        "the C64's lower-case set"
    );
    assert_eq!(
        typing::<Key>('\n', LetterCase::Capitals),
        Some((Key::RETURN, false)),
        "a line ends with RETURN"
    );
    assert_eq!(
        typing::<Key>('"', LetterCase::Capitals),
        Some((Key::DIGIT_2, true))
    );
    assert_eq!(
        typing::<Key>('£', LetterCase::Capitals),
        Some((Key::UNDERSCORE, true))
    );
    assert_eq!(typing::<Key>('é', LetterCase::Capitals), None);
}

/// The Electron's FUNC is the host's Alt either way, a function key is FUNC
/// with its digit, and Caps Lock is CAPS LK/FUNC: with SHIFT by character, so
/// it toggles CAPS LOCK, and alone in its place.
#[test]
fn the_electrons_func_rides_on_alt_and_its_soft_keys_on_the_function_keys() {
    type Electron = super::Keyboard<EKey>;
    let pressed = |key| super::Press::Key(key, true);
    let released = |key| super::Press::Key(key, false);
    let mut logical = Electron::new(Mapping::Logical);
    assert_eq!(
        presses(&mut logical, &[(0xE2, None, true), (0xE2, None, false)]),
        [pressed(EKey::FUNC), released(EKey::FUNC)]
    );
    assert_eq!(
        presses(
            &mut logical,
            &[
                (0x3A, Some(named(NamedKeyCode::F1)), true),
                (0x3A, Some(named(NamedKeyCode::F1)), false)
            ]
        ),
        [
            pressed(EKey::FUNC),
            pressed(EKey::DIGIT_1),
            released(EKey::DIGIT_1),
            released(EKey::FUNC)
        ]
    );
    assert_eq!(
        presses(&mut logical, &[(0x39, None, true), (0x39, None, false)]),
        [
            pressed(EKey::SHIFT),
            pressed(EKey::FUNC),
            released(EKey::FUNC),
            released(EKey::SHIFT)
        ]
    );
    let mut physical = Electron::new(Mapping::Physical);
    assert_eq!(
        presses(&mut physical, &[(0x39, None, true), (0x39, None, false)]),
        [pressed(EKey::FUNC), released(EKey::FUNC)]
    );
    assert_eq!(
        presses(&mut physical, &[(0x4D, None, true)]),
        [pressed(EKey::COPY)],
        "COPY on End"
    );
    assert_eq!(
        typing::<EKey>('a', LetterCase::Capitals),
        Some((EKey::A, true))
    );
    assert_eq!(
        typing::<EKey>('*', LetterCase::Capitals),
        Some((EKey::COLON, true))
    );
}

type C64Keyboard = super::Keyboard<Keycap>;
type C64Press = super::Press<Keycap>;

const fn c64(key: C64Key, down: bool) -> C64Press {
    C64Press::Key(Keycap::Matrix(key), down)
}

#[test]
fn a_c64_types_logically_with_shifted_keys_its_own() {
    let mut keyboard = C64Keyboard::new(Mapping::Logical);
    assert_eq!(
        presses(
            &mut keyboard,
            &[(LEFT_SHIFT, None, true), (0x1F, Some(ch('"')), true)]
        ),
        [c64(C64Key::LEFT_SHIFT, true), c64(C64Key::DIGIT_2, true)],
        "a quote is SHIFT and 2"
    );
    let mut keyboard = C64Keyboard::new(Mapping::Logical);
    assert_eq!(
        presses(
            &mut keyboard,
            &[
                (0x52, Some(named(NamedKeyCode::Up)), true),
                (0x52, Some(named(NamedKeyCode::Up)), false)
            ]
        ),
        [
            c64(C64Key::LEFT_SHIFT, true),
            c64(C64Key::CURSOR_DOWN, true),
            c64(C64Key::CURSOR_DOWN, false),
            c64(C64Key::LEFT_SHIFT, false)
        ],
        "up is SHIFT with the cursor-down key"
    );
    assert_eq!(
        presses(
            &mut keyboard,
            &[(0x45, Some(named(NamedKeyCode::F12)), true)]
        ),
        [C64Press::Key(Keycap::Restore, true)],
        "F12 is RESTORE"
    );
    assert_eq!(
        presses(
            &mut keyboard,
            &[(0x3B, Some(named(NamedKeyCode::F2)), true)]
        ),
        [c64(C64Key::LEFT_SHIFT, true), c64(C64Key::F1, true)],
        "f2 is SHIFT with f1"
    );
}

#[test]
fn a_c64_key_in_place_takes_the_position_its_own_has() {
    let mut keyboard = C64Keyboard::new(Mapping::Physical);
    assert_eq!(
        presses(&mut keyboard, &[(0x39, None, true), (0x39, None, false)]),
        [c64(C64Key::RUN_STOP, true), c64(C64Key::RUN_STOP, false)],
        "RUN/STOP where Caps Lock is"
    );
    assert_eq!(
        presses(&mut keyboard, &[(0x52, None, true)]),
        [
            c64(C64Key::RIGHT_SHIFT, true),
            c64(C64Key::CURSOR_DOWN, true)
        ],
        "up holds the right SHIFT, leaving the host's Shift its left"
    );
    assert_eq!(
        presses(&mut keyboard, &[(0x2F, None, true)]),
        [c64(C64Key::AT, true)]
    );
}

#[test]
fn the_keypad_is_a_joystick_and_lets_every_other_key_by() {
    let mut stick = Stick::new();
    let pad = |usage, pressed| edge(usage, None, pressed);
    assert_eq!(stick.edge(&pad(0x60, true)), Some(joystick::UP));
    assert_eq!(
        stick.edge(&pad(0x5E, true)),
        Some(joystick::UP | joystick::RIGHT)
    );
    assert_eq!(
        stick.edge(&pad(0x62, true)),
        Some(joystick::UP | joystick::RIGHT | joystick::FIRE)
    );
    assert_eq!(
        stick.edge(&pad(0x60, false)),
        Some(joystick::RIGHT | joystick::FIRE)
    );
    assert_eq!(
        stick.edge(&pad(0x04, true)),
        None,
        "a letter is the keyboard's"
    );
    assert_eq!(stick.release_all(), Some(0));
    assert_eq!(stick.release_all(), None, "nothing left held");
}

#[test]
fn a_tap_is_held_and_a_release_kept_for_the_least_hold() {
    const LEAST: u64 = 50;
    let mut edges = Edges::new();
    assert_eq!(edges.key(3, true, 100, LEAST), 100, "a first press at once");
    assert_eq!(
        edges.key(3, false, 110, LEAST),
        150,
        "a tap held long enough"
    );
    assert_eq!(
        edges.key(3, true, 150, LEAST),
        200,
        "pressed again only once the release has lasted"
    );
    assert_eq!(
        edges.key(3, false, 400, LEAST),
        400,
        "a long press as it was"
    );
    assert_eq!(
        edges.key(4, true, 160, LEAST),
        160,
        "another key is not held up"
    );
}

#[test]
fn a_stick_line_is_held_and_released_as_a_key_is() {
    use crate::machine::c64::joystick::{FIRE, UP};
    const LEAST: u64 = 50;
    let mut edges = Edges::new();
    assert_eq!(edges.stick(1, UP, 0, LEAST), 0);
    assert_eq!(
        edges.stick(1, UP | FIRE, 10, LEAST),
        10,
        "a new line at once"
    );
    assert_eq!(edges.stick(1, FIRE, 20, LEAST), 50, "up held long enough");
    assert_eq!(edges.stick(1, 0, 60, LEAST), 60, "fire held since 10");
    assert_eq!(edges.stick(1, FIRE, 70, LEAST), 110, "fire's release kept");
    assert_eq!(edges.stick(2, FIRE, 5, LEAST), 5, "no third port");
}
