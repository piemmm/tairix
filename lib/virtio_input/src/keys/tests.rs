//! The virtio keyboard's key edges: each `evdev` keycode named as its
//! physical key, and — read through a seat as the kernel reads them — typing
//! what the US layout makes of it.

use super::*;
use tairix_abi::driver::input::InputEventKind;
use tairix_abi::input::{KeyInput, KeyValue, NamedKeyCode};
use tairix_input::KeyboardState;
use tairix_keymap::key_record;

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

/// A seat reading this keyboard's edges.
struct Seat(KeyboardState);

impl Seat {
    fn new() -> Self {
        Self(KeyboardState::new())
    }

    /// The record keycode `keycode` going down (`value` 1) or up (0) is read
    /// as, if it is an edge at all.
    fn feed(&mut self, keycode: u16, value: i32) -> Option<KeyInput> {
        let edge = key_edge(key(keycode, value))?;
        key_record(&mut self.0, edge.key, edge.pressed)
    }

    /// Hold `keycode` down, answering its record.
    fn hold(&mut self, keycode: u16) -> KeyInput {
        self.feed(keycode, 1).expect("a key not already down")
    }

    /// Press and let go of `keycode`, answering what the press typed.
    fn tap(&mut self, keycode: u16) -> Option<KeyValue> {
        let record = self.hold(keycode);
        self.feed(keycode, 0).expect("a key down");
        record.key()
    }
}

#[test]
fn plain_letter_press_and_release_resolve_to_the_char() {
    let mut seat = Seat::new();
    // KEY_H = 35.
    assert_eq!(seat.hold(35).key(), Some(KeyValue::Char('h')));
    let released = seat.feed(35, 0).expect("release record");
    assert!(!released.is_press());
    assert_eq!(released.key(), Some(KeyValue::Char('h')));
}

#[test]
fn shift_held_uppercases_the_letter_and_is_an_edge_of_its_own() {
    let mut seat = Seat::new();
    // Left Shift (42) types nothing and is read as its own edge.
    let shift = seat.hold(42);
    assert_eq!(
        (shift.key(), shift.physical(), shift.modifiers().shift),
        (None, PhysicalKey::LEFT_SHIFT, true)
    );
    // KEY_A = 30 with shift held → 'A', and the record carries shift.
    let record = seat.hold(30);
    assert_eq!(record.key(), Some(KeyValue::Char('A')));
    assert!(record.modifiers().shift);
}

#[test]
fn caps_lock_toggles_letter_case_and_shift_xor_caps_holds() {
    let mut seat = Seat::new();
    // Caps Lock (58) is an edge that types nothing.
    assert_eq!(seat.tap(58), None);
    assert_eq!(seat.tap(30), Some(KeyValue::Char('A')));
    // Shift while caps is on → back to lowercase (shift XOR caps).
    let _ = seat.hold(42);
    assert_eq!(seat.tap(30), Some(KeyValue::Char('a')));
}

#[test]
fn shifted_digit_resolves_to_its_symbol() {
    let mut seat = Seat::new();
    let _ = seat.hold(42);
    assert_eq!(seat.tap(2), Some(KeyValue::Char('!'))); // KEY_1
}

#[test]
fn named_keys_resolve() {
    let mut seat = Seat::new();
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
            seat.tap(keycode),
            Some(KeyValue::Named(named)),
            "keycode {keycode}"
        );
    }
}

#[test]
fn function_keys_span_f1_through_f12() {
    let mut seat = Seat::new();
    // F1 = 59, F10 = 68 (contiguous), F11 = 87, F12 = 88.
    for (keycode, named) in [
        (59, NamedKeyCode::F1),
        (68, NamedKeyCode::F10),
        (87, NamedKeyCode::F11),
        (88, NamedKeyCode::F12),
    ] {
        assert_eq!(seat.tap(keycode), Some(KeyValue::Named(named)));
    }
}

#[test]
fn left_right_modifier_pair_stays_held_until_both_release() {
    let mut seat = Seat::new();
    assert!(seat.hold(29).modifiers().ctrl); // left ctrl down
    assert!(seat.hold(97).modifiers().ctrl); // right ctrl down
    assert!(
        seat.feed(29, 0).expect("read").modifiers().ctrl,
        "the right one holds it"
    );
    assert!(seat.hold(46).modifiers().ctrl); // KEY_C
    let _ = seat.feed(46, 0);
    assert!(!seat.feed(97, 0).expect("read").modifiers().ctrl);
    assert!(!seat.hold(46).modifiers().ctrl);
}

#[test]
fn numeric_keypad_honours_num_lock() {
    let mut seat = Seat::new();
    // Num Lock off: KEY_KP7 (71) is the Home navigation key, and KEY_KP5 (76)
    // types nothing but is still read.
    assert_eq!(seat.tap(71), Some(KeyValue::Named(NamedKeyCode::Home)));
    assert_eq!(seat.tap(76), None);
    // Turn Num Lock on (69): now KP7 is the digit '7' and KP5 is '5'.
    assert_eq!(seat.tap(69), None);
    assert_eq!(seat.tap(71), Some(KeyValue::Char('7')));
    assert_eq!(seat.tap(76), Some(KeyValue::Char('5')));
}

#[test]
fn non_key_events_and_unknown_or_repeat_edges_are_no_edge() {
    // A pointer event is not a key edge.
    let pointer = InputEvent {
        kind: InputEventKind::Pointer,
        reserved0: 0,
        code: 0,
        value: 5,
    };
    assert_eq!(key_edge(pointer), None);
    // An unmapped keycode.
    assert_eq!(key_edge(key(0xFFFF, 1)), None);
    // A key-repeat edge (`value == 2`) carries no press/release.
    assert_eq!(key_edge(key(35, 2)), None);
    assert_eq!(
        key_edge(key(35, 1)),
        Some(KeyEdge {
            key: PhysicalKey::from_usage(0x0B).expect("a key"),
            pressed: true
        })
    );
}

#[test]
fn a_key_record_names_the_key_pressed_whatever_it_typed() {
    let mut seat = Seat::new();
    for (keycode, physical) in [
        (30u16, PhysicalKey::A),
        (4, PhysicalKey::DIGIT_3),
        (5, PhysicalKey::DIGIT_4),
        (6, PhysicalKey::DIGIT_5),
        (57, PhysicalKey::SPACE),
        (1, PhysicalKey::ESCAPE),
    ] {
        let record = seat.hold(keycode);
        assert_eq!(record.physical(), physical, "keycode {keycode}");
        let _ = seat.feed(keycode, 0);
    }
    // Under Meta, Shift, Ctrl and Alt the 3 key types `#` and is still the
    // 3 key.
    for modifier in [125u16, 42, 29, 56] {
        let _ = seat.hold(modifier);
    }
    let record = seat.hold(4);
    assert_eq!(record.key(), Some(KeyValue::Char('#')));
    assert_eq!(record.physical(), PhysicalKey::DIGIT_3);
    let held = record.modifiers();
    assert!(held.meta && held.shift && held.ctrl && held.alt);
}

#[test]
fn the_keycode_table_ascends_so_a_keycode_is_found_by_halving() {
    assert!(KEYCODES.windows(2).all(|pair| pair[0].0 < pair[1].0));
}

#[test]
fn the_modifier_lock_and_system_keycodes_name_their_physical_keys() {
    for (keycode, usage) in [
        (42u16, 0xE1),
        (54, 0xE5),
        (29, 0xE0),
        (97, 0xE4),
        (56, 0xE2),
        (100, 0xE6),
        (125, 0xE3),
        (126, 0xE7),
        (58, 0x39),
        (69, 0x53),
        (70, 0x47),
        (99, 0x46),
        (119, 0x48),
        (86, 0x64),
        (117, 0x67),
        (127, 0x65),
    ] {
        assert_eq!(
            physical_key(keycode),
            PhysicalKey::from_usage(usage),
            "keycode {keycode}"
        );
    }
}

#[test]
fn every_keycode_the_table_names_is_an_edge_of_that_key_and_no_other_is_one() {
    for keycode in 0..=KEY_MAX {
        let physical = physical_key(keycode);
        for (value, pressed) in [(1, true), (0, false)] {
            let edge = key_edge(key(keycode, value));
            let expected = physical.map(|key| KeyEdge { key, pressed });
            assert_eq!(edge, expected, "keycode {keycode}");
        }
    }
}

/// Type `keycodes` on a fresh seat, with Num Lock on when `num_lock`,
/// checking each types the next character of `expected`.
fn assert_types(keycodes: &[u16], num_lock: bool, expected: &str) {
    let mut seat = Seat::new();
    if num_lock {
        let _ = seat.tap(69);
    }
    assert_eq!(keycodes.len(), expected.chars().count());
    for (&keycode, want) in keycodes.iter().zip(expected.chars()) {
        assert_eq!(
            seat.tap(keycode),
            Some(KeyValue::Char(want)),
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
    let mut seat = Seat::new();
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
            seat.tap(keycode),
            Some(KeyValue::Named(named)),
            "keycode {keycode}"
        );
    }
}
