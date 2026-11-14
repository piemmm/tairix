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
fn pasted_text_takes_shift_from_its_case_and_the_machines_caps_lock() {
    assert_eq!(
        typing::<Key>('a', true),
        Some((Key::A, true)),
        "CAPS LOCK on, SHIFT for lower case"
    );
    assert_eq!(typing::<Key>('A', true), Some((Key::A, false)));
    assert_eq!(typing::<Key>('a', false), Some((Key::A, false)));
    assert_eq!(typing::<Key>('A', false), Some((Key::A, true)));
    assert_eq!(
        typing::<Key>('\n', true),
        Some((Key::RETURN, false)),
        "a line ends with RETURN"
    );
    assert_eq!(typing::<Key>('"', true), Some((Key::DIGIT_2, true)));
    assert_eq!(typing::<Key>('£', true), Some((Key::UNDERSCORE, true)));
    assert_eq!(typing::<Key>('é', true), None);
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
    assert_eq!(typing::<EKey>('a', true), Some((EKey::A, true)));
    assert_eq!(typing::<EKey>('*', true), Some((EKey::COLON, true)));
}
