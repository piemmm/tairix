//! Unit tests for the virtio-input keyboard console producer.

use super::*;
use tairix_abi::driver::input::InputEventKind;
use tairix_abi::input::{KeyInput, KeyValue, Modifiers as AbiModifiers, NamedKeyCode};

/// The highest `evdev` keycode (`KEY_MAX`).
const KEY_MAX: u16 = 0x2FF;

/// Build one keyboard [`InputEvent`] for `keycode` with the given `value`
/// (`1` = press, `0` = release, `2` = repeat).
fn key(keycode: u16, value: i32) -> InputEvent {
    InputEvent {
        kind: InputEventKind::Key,
        reserved0: 0,
        code: keycode,
        value,
    }
}

/// The [`KeyValue`] a `Pressed`/`Released` record carries, failing the test
/// if `feed` produced nothing.
fn pressed_value(record: Option<KeyInput>) -> KeyValue {
    match record.expect("a record was produced") {
        KeyInput::Pressed { key, .. } => key,
        other => panic!("expected a press record, got {other:?}"),
    }
}

/// The held set a modifier edge reported changing to, or `None` when the
/// collapsed set did not change.
fn modifier_set(record: Option<KeyInput>) -> Option<AbiModifiers> {
    match record {
        None => None,
        Some(KeyInput::ModifiersChanged { modifiers }) => Some(modifiers),
        Some(other) => panic!("a modifier edge produced {other:?}"),
    }
}

#[test]
fn plain_letter_press_and_release_resolve_to_the_char() {
    let mut kbd = VirtioKeyboardConsole::new();
    // KEY_H = 35.
    assert_eq!(pressed_value(kbd.feed(key(35, 1))), KeyValue::Char('h'));
    match kbd.feed(key(35, 0)).expect("release record") {
        KeyInput::Released { key, .. } => assert_eq!(key, KeyValue::Char('h')),
        other => panic!("expected a release record, got {other:?}"),
    }
}

#[test]
fn shift_held_uppercases_the_letter_and_reports_its_own_edge() {
    let mut kbd = VirtioKeyboardConsole::new();
    // Pressing Left Shift (42) names no key, but reports the held set.
    assert_eq!(
        modifier_set(kbd.feed(key(42, 1))),
        Some(AbiModifiers {
            shift: true,
            ..AbiModifiers::default()
        })
    );
    // KEY_A = 30 with shift held → 'A', and the record carries shift.
    let record = kbd.feed(key(30, 1)).expect("record");
    assert_eq!(record.key(), Some(KeyValue::Char('A')));
    assert!(record.modifiers().shift);
}

#[test]
fn caps_lock_toggles_letter_case_and_shift_xor_caps_holds() {
    let mut kbd = VirtioKeyboardConsole::new();
    // Caps Lock (58) press toggles state, no record.
    assert_eq!(kbd.feed(key(58, 1)), None);
    assert_eq!(pressed_value(kbd.feed(key(30, 1))), KeyValue::Char('A'));
    // Shift while caps is on → back to lowercase (shift XOR caps).
    assert!(modifier_set(kbd.feed(key(42, 1))).is_some());
    assert_eq!(pressed_value(kbd.feed(key(30, 1))), KeyValue::Char('a'));
}

#[test]
fn shifted_digit_resolves_to_its_symbol() {
    let mut kbd = VirtioKeyboardConsole::new();
    assert!(modifier_set(kbd.feed(key(42, 1))).is_some()); // shift down
    assert_eq!(pressed_value(kbd.feed(key(2, 1))), KeyValue::Char('!')); // KEY_1
}

#[test]
fn named_keys_resolve() {
    let mut kbd = VirtioKeyboardConsole::new();
    for (keycode, named) in [
        (28u16, NamedKeyCode::Enter),
        (96, NamedKeyCode::Enter), // keypad Enter
        (1, NamedKeyCode::Escape),
        (14, NamedKeyCode::Backspace),
        (15, NamedKeyCode::Tab),
        (103, NamedKeyCode::Up),
        (105, NamedKeyCode::Left),
        (111, NamedKeyCode::Delete),
    ] {
        assert_eq!(
            pressed_value(kbd.feed(key(keycode, 1))),
            KeyValue::Named(named),
            "keycode {keycode}"
        );
    }
}

#[test]
fn function_keys_span_f1_through_f12() {
    let mut kbd = VirtioKeyboardConsole::new();
    // F1 = 59, F10 = 68 (contiguous), F11 = 87, F12 = 88.
    assert_eq!(
        pressed_value(kbd.feed(key(59, 1))),
        KeyValue::Named(NamedKeyCode::F1)
    );
    assert_eq!(
        pressed_value(kbd.feed(key(68, 1))),
        KeyValue::Named(NamedKeyCode::F10)
    );
    assert_eq!(
        pressed_value(kbd.feed(key(87, 1))),
        KeyValue::Named(NamedKeyCode::F11)
    );
    assert_eq!(
        pressed_value(kbd.feed(key(88, 1))),
        KeyValue::Named(NamedKeyCode::F12)
    );
}

#[test]
fn left_right_modifier_pair_stays_held_until_both_release() {
    let mut kbd = VirtioKeyboardConsole::new();
    // Hold both control keys, release the left one, control is still held —
    // so only the first press and the last release are observable changes.
    assert!(modifier_set(kbd.feed(key(29, 1))).is_some()); // left ctrl down
    assert_eq!(modifier_set(kbd.feed(key(97, 1))), None); // right ctrl down
    assert_eq!(modifier_set(kbd.feed(key(29, 0))), None); // left ctrl up
    let record = kbd.feed(key(46, 1)).expect("record"); // KEY_C
    assert!(record.modifiers().ctrl);
    // Releasing the right one too clears control.
    assert_eq!(
        modifier_set(kbd.feed(key(97, 0))),
        Some(AbiModifiers::default())
    );
    let record = kbd.feed(key(46, 1)).expect("record");
    assert!(!record.modifiers().ctrl);
}

#[test]
fn numeric_keypad_honours_num_lock() {
    let mut kbd = VirtioKeyboardConsole::new();
    // Num Lock off: KEY_KP7 (71) is the Home navigation key.
    assert_eq!(
        pressed_value(kbd.feed(key(71, 1))),
        KeyValue::Named(NamedKeyCode::Home)
    );
    // KEY_KP5 (76) sends nothing with Num Lock off.
    assert_eq!(kbd.feed(key(76, 1)), None);
    // Turn Num Lock on (69): now KP7 is the digit '7' and KP5 is '5'.
    assert_eq!(kbd.feed(key(69, 1)), None);
    assert_eq!(pressed_value(kbd.feed(key(71, 1))), KeyValue::Char('7'));
    assert_eq!(pressed_value(kbd.feed(key(76, 1))), KeyValue::Char('5'));
}

#[test]
fn non_key_events_and_unknown_or_repeat_edges_produce_nothing() {
    let mut kbd = VirtioKeyboardConsole::new();
    // A pointer event is not a key edge.
    let pointer = InputEvent {
        kind: InputEventKind::Pointer,
        reserved0: 0,
        code: 0,
        value: 5,
    };
    assert_eq!(kbd.feed(pointer), None);
    // An unmapped keycode.
    assert_eq!(kbd.feed(key(0xFFFF, 1)), None);
    // A key-repeat edge (`value == 2`) carries no press/release.
    assert_eq!(kbd.feed(key(35, 2)), None);
}

#[test]
fn a_modifier_edge_reports_only_an_observable_change() {
    let mut kbd = VirtioKeyboardConsole::new();
    // Left Shift down is a change; the right shift joining and leaving the
    // chord is not, and shift stays held throughout.
    assert_eq!(
        modifier_set(kbd.feed(key(42, 1))),
        Some(AbiModifiers {
            shift: true,
            ..AbiModifiers::default()
        })
    );
    assert_eq!(modifier_set(kbd.feed(key(54, 1))), None);
    assert_eq!(modifier_set(kbd.feed(key(54, 0))), None);
    assert_eq!(
        modifier_set(kbd.feed(key(42, 0))),
        Some(AbiModifiers::default())
    );
}

#[test]
fn a_key_record_names_the_key_pressed_whatever_it_typed() {
    let mut kbd = VirtioKeyboardConsole::new();
    for (keycode, physical) in [
        (30u16, PhysicalKey::A),
        (4, PhysicalKey::DIGIT_3),
        (5, PhysicalKey::DIGIT_4),
        (6, PhysicalKey::DIGIT_5),
        (57, PhysicalKey::SPACE),
        (1, PhysicalKey::ESCAPE),
    ] {
        let record = kbd.feed(key(keycode, 1)).expect("a key record");
        assert_eq!(record.physical(), Some(physical), "keycode {keycode}");
    }
    // Under Meta, Shift, Ctrl and Alt the 3 key types `#` and is still the
    // 3 key.
    for modifier in [125u16, 42, 29, 56] {
        assert!(modifier_set(kbd.feed(key(modifier, 1))).is_some());
    }
    let record = kbd.feed(key(4, 1)).expect("a key record");
    assert_eq!(record.key(), Some(KeyValue::Char('#')));
    assert_eq!(record.physical(), Some(PhysicalKey::DIGIT_3));
    let held = record.modifiers();
    assert!(held.meta && held.shift && held.ctrl && held.alt);
}

#[test]
fn the_keycode_table_ascends_so_a_keycode_is_found_by_halving() {
    assert!(KEYCODES.windows(2).all(|pair| pair[0].0 < pair[1].0));
}

#[test]
fn the_modifier_and_lock_keycodes_name_their_physical_keys() {
    for (keycode, physical) in [
        (42u16, PhysicalKey::LEFT_SHIFT),
        (54, PhysicalKey::RIGHT_SHIFT),
        (29, PhysicalKey::LEFT_CTRL),
        (97, PhysicalKey::RIGHT_CTRL),
        (56, PhysicalKey::LEFT_ALT),
        (100, PhysicalKey::RIGHT_ALT),
        (125, PhysicalKey::LEFT_GUI),
        (126, PhysicalKey::RIGHT_GUI),
        (58, PhysicalKey::CAPS_LOCK),
        (69, PhysicalKey::NUM_LOCK),
    ] {
        assert_eq!(physical_key(keycode), physical, "keycode {keycode}");
    }
}

#[test]
fn the_keycode_table_names_exactly_the_keys_the_console_reads() {
    for keycode in 0..=KEY_MAX {
        let physical = physical_key(keycode);
        let mut read = physical == PhysicalKey::CAPS_LOCK || physical == PhysicalKey::NUM_LOCK;
        for num_lock in [false, true] {
            let mut kbd = VirtioKeyboardConsole::new();
            if num_lock {
                let _ = kbd.feed(key(69, 1));
            }
            if let Some(record) = kbd.feed(key(keycode, 1)) {
                read = true;
                assert!(record.physical().is_none_or(|named| named == physical));
            }
        }
        assert_eq!(physical != PhysicalKey::UNKNOWN, read, "keycode {keycode}");
    }
}

/// Type `keycodes` on a fresh keyboard, with Num Lock on when `num_lock`,
/// checking each types the next character of `expected`.
fn assert_types(keycodes: &[u16], num_lock: bool, expected: &str) {
    let mut kbd = VirtioKeyboardConsole::new();
    if num_lock {
        let _ = kbd.feed(key(69, 1));
    }
    assert_eq!(keycodes.len(), expected.chars().count());
    for (&keycode, want) in keycodes.iter().zip(expected.chars()) {
        assert_eq!(
            pressed_value(kbd.feed(key(keycode, 1))),
            KeyValue::Char(want),
            "keycode {keycode}"
        );
    }
}

#[test]
fn every_printable_keycode_types_its_us_character() {
    assert_types(
        &[16, 17, 18, 19, 20, 21, 22, 23, 24, 25],
        false,
        "qwertyuiop",
    );
    assert_types(&[30, 31, 32, 33, 34, 35, 36, 37, 38], false, "asdfghjkl");
    assert_types(&[44, 45, 46, 47, 48, 49, 50], false, "zxcvbnm");
    assert_types(&[2, 3, 4, 5, 6, 7, 8, 9, 10, 11], false, "1234567890");
    assert_types(
        &[12, 13, 26, 27, 39, 40, 41, 43, 51, 52, 53, 57],
        false,
        "-=[];'`\\,./ ",
    );
    assert_types(
        &[98, 55, 74, 78, 71, 72, 73, 75, 76, 77, 79, 80, 81, 82, 83],
        true,
        "/*-+7894561230.",
    );
}

#[test]
fn every_navigation_and_function_keycode_names_its_key() {
    let mut kbd = VirtioKeyboardConsole::new();
    for (keycode, named) in [
        (102u16, NamedKeyCode::Home),
        (104, NamedKeyCode::PageUp),
        (106, NamedKeyCode::Right),
        (107, NamedKeyCode::End),
        (108, NamedKeyCode::Down),
        (109, NamedKeyCode::PageDown),
        (110, NamedKeyCode::Insert),
        (60, NamedKeyCode::F2),
        (61, NamedKeyCode::F3),
        (62, NamedKeyCode::F4),
        (63, NamedKeyCode::F5),
        (64, NamedKeyCode::F6),
        (65, NamedKeyCode::F7),
        (66, NamedKeyCode::F8),
        (67, NamedKeyCode::F9),
        // The keypad with Num Lock off.
        (72, NamedKeyCode::Up),
        (73, NamedKeyCode::PageUp),
        (75, NamedKeyCode::Left),
        (77, NamedKeyCode::Right),
        (79, NamedKeyCode::End),
        (80, NamedKeyCode::Down),
        (81, NamedKeyCode::PageDown),
        (82, NamedKeyCode::Insert),
        (83, NamedKeyCode::Delete),
    ] {
        assert_eq!(
            pressed_value(kbd.feed(key(keycode, 1))),
            KeyValue::Named(named),
            "keycode {keycode}"
        );
    }
}
