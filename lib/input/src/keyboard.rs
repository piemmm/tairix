//! How a seat reads its keyboards: which keys are down, the held modifiers,
//! the caps and num locks, and the US layout.
//!
//! A keyboard driver names each edge's [`PhysicalKey`] — a HID usage is one
//! already, an `evdev` keycode goes through its driver's one table — and the
//! seat feeds every edge from every one of its keyboards to one
//! [`KeyboardState::edge`], so the keyboards share their modifiers and locks
//! and no two read the same key differently.

use tairix_abi::input::PhysicalKey;
use tairix_inline::BitSet256;

use crate::{Key, ModifierKey, ModifierSide, ModifierState, Modifiers, NamedKey};

/// What one key edge was read as: what it typed, if anything — a release
/// typing what its press did — and the modifiers held once it had gone down
/// or come up.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct KeyRead {
    /// What the key typed: nothing for a modifier, a lock, or a key the
    /// layout names no value for.
    pub key: Option<Key>,
    /// The modifiers held once the edge was taken.
    pub modifiers: Modifiers,
}

/// A seat's keyboards as their key edges have left them: the keys down, the
/// modifier keys held, and the caps and num locks, both off until first
/// pressed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct KeyboardState {
    /// The HID usage of every key down.
    down: BitSet256,
    /// Of the keys down, the ones pressed under Shift, Caps Lock and Num Lock,
    /// so a release reads what its press typed whatever changed meanwhile.
    pressed_shifted: BitSet256,
    pressed_caps: BitSet256,
    pressed_num: BitSet256,
    modifiers: ModifierState,
    caps_lock: bool,
    num_lock: bool,
}

impl KeyboardState {
    /// A keyboard with no key down and both locks off.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            down: BitSet256::EMPTY,
            pressed_shifted: BitSet256::EMPTY,
            pressed_caps: BitSet256::EMPTY,
            pressed_num: BitSet256::EMPTY,
            modifiers: ModifierState::new(),
            caps_lock: false,
            num_lock: false,
        }
    }

    /// The modifiers held.
    #[must_use]
    pub const fn modifiers(&self) -> Modifiers {
        self.modifiers.modifiers()
    }

    /// Take one edge of `key`, a press when `pressed`, answering how it was
    /// read.
    ///
    /// Every key that goes down is read, and comes up once: a press of a key
    /// already down is its keyboard's own repeat and a release of one not down
    /// was never seen pressed, and both answer nothing, as does a key the page
    /// names none. A modifier key and a lock type nothing; a lock toggles on
    /// its press. Any other key's press types what the US layout makes of it
    /// under the shift key and the locks, or nothing where the layout names
    /// none — Print Screen, or keypad 5 with Num Lock off — and its release
    /// reads as its press did.
    pub fn edge(&mut self, key: PhysicalKey, pressed: bool) -> Option<KeyRead> {
        let usage = key.usage();
        if self.down.contains(usage) == pressed {
            return None;
        }
        if pressed {
            self.down.insert(usage);
        } else {
            self.down.remove(usage);
        }
        let key = if let Some((modifier, side)) = modifier_of(key) {
            if pressed {
                self.modifiers.press(modifier, side);
            } else {
                self.modifiers.release(modifier, side);
            }
            None
        } else if key == PhysicalKey::CAPS_LOCK {
            self.caps_lock ^= pressed;
            None
        } else if key == PhysicalKey::NUM_LOCK {
            self.num_lock ^= pressed;
            None
        } else {
            let (shift, caps, num) = self.read_under(usage, pressed);
            us_layout(usage, shift, caps, num)
        };
        Some(KeyRead {
            key,
            modifiers: self.modifiers(),
        })
    }

    /// The shift key and locks a press of `usage` is read under — those held
    /// now, kept for its release — or, for its release, those its press was.
    fn read_under(&mut self, usage: u16, pressed: bool) -> (bool, bool, bool) {
        let sets = [
            &mut self.pressed_shifted,
            &mut self.pressed_caps,
            &mut self.pressed_num,
        ];
        if pressed {
            let now = (
                self.modifiers.modifiers().shift,
                self.caps_lock,
                self.num_lock,
            );
            for (set, on) in sets.into_iter().zip([now.0, now.1, now.2]) {
                if on {
                    set.insert(usage);
                }
            }
            now
        } else {
            let [shift, caps, num] = sets.map(|set| {
                let was = set.contains(usage);
                set.remove(usage);
                was
            });
            (shift, caps, num)
        }
    }
}

/// The modifier `key` is, or `None` for any other key.
fn modifier_of(key: PhysicalKey) -> Option<(ModifierKey, ModifierSide)> {
    Some(match key {
        PhysicalKey::LEFT_CTRL => (ModifierKey::Ctrl, ModifierSide::Left),
        PhysicalKey::LEFT_SHIFT => (ModifierKey::Shift, ModifierSide::Left),
        PhysicalKey::LEFT_ALT => (ModifierKey::Alt, ModifierSide::Left),
        PhysicalKey::LEFT_GUI => (ModifierKey::Meta, ModifierSide::Left),
        PhysicalKey::RIGHT_CTRL => (ModifierKey::Ctrl, ModifierSide::Right),
        PhysicalKey::RIGHT_SHIFT => (ModifierKey::Shift, ModifierSide::Right),
        PhysicalKey::RIGHT_ALT => (ModifierKey::Alt, ModifierSide::Right),
        PhysicalKey::RIGHT_GUI => (ModifierKey::Meta, ModifierSide::Right),
        _ => return None,
    })
}

/// The [`Key`] the US layout gives HID usage `usage` under the shift key and
/// the caps and num locks, or `None` where it names none.
fn us_layout(usage: u16, shift: bool, caps: bool, num: bool) -> Option<Key> {
    if let Some(letter) = letter(usage, shift, caps) {
        return Some(Key::Char(letter));
    }
    if let Some(ch) = printable(usage, shift) {
        return Some(Key::Char(ch));
    }
    if let Some(named) = named(usage) {
        return Some(Key::Named(named));
    }
    keypad(usage, num)
}

/// A letter (`0x04` is `A`, `0x1D` is `Z`), upper case under shift or caps
/// lock but not both.
fn letter(usage: u16, shift: bool, caps: bool) -> Option<char> {
    if (0x04..=0x1D).contains(&usage) {
        let offset = u8::try_from(usage - 0x04).ok()?;
        let lower = b'a' + offset;
        let byte = if shift ^ caps {
            lower.to_ascii_uppercase()
        } else {
            lower
        };
        return Some(char::from(byte));
    }
    None
}

/// A digit, the space bar or a punctuation key, its shifted glyph under shift.
fn printable(usage: u16, shift: bool) -> Option<char> {
    // (usage, unshifted, shifted). The US ANSI layout's non-letter printables.
    const TABLE: &[(u16, char, char)] = &[
        (0x1E, '1', '!'),
        (0x1F, '2', '@'),
        (0x20, '3', '#'),
        (0x21, '4', '$'),
        (0x22, '5', '%'),
        (0x23, '6', '^'),
        (0x24, '7', '&'),
        (0x25, '8', '*'),
        (0x26, '9', '('),
        (0x27, '0', ')'),
        (0x2C, ' ', ' '),
        (0x2D, '-', '_'),
        (0x2E, '=', '+'),
        (0x2F, '[', '{'),
        (0x30, ']', '}'),
        (0x31, '\\', '|'),
        (0x33, ';', ':'),
        (0x34, '\'', '"'),
        (0x35, '`', '~'),
        (0x36, ',', '<'),
        (0x37, '.', '>'),
        (0x38, '/', '?'),
    ];
    for &(code, unshifted, shifted) in TABLE {
        if code == usage {
            return Some(if shift { shifted } else { unshifted });
        }
    }
    None
}

/// An editing, navigation, function or main-block control key.
fn named(usage: u16) -> Option<NamedKey> {
    Some(match usage {
        0x28 => NamedKey::Enter,
        0x29 => NamedKey::Escape,
        0x2A => NamedKey::Backspace,
        0x2B => NamedKey::Tab,
        0x49 => NamedKey::Insert,
        0x4A => NamedKey::Home,
        0x4B => NamedKey::PageUp,
        0x4C => NamedKey::Delete,
        0x4D => NamedKey::End,
        0x4E => NamedKey::PageDown,
        0x4F => NamedKey::Right,
        0x50 => NamedKey::Left,
        0x51 => NamedKey::Down,
        0x52 => NamedKey::Up,
        0x3A..=0x45 => NamedKey::Function {
            number: u8::try_from(usage - 0x3A + 1).ok()?,
        },
        _ => return None,
    })
}

/// A keypad key: the digits, `.` and the operators with Num Lock on; with it
/// off the digit keys are the navigation cluster (`KP1` End, `KP8` Up, …) and
/// `KP5` sends nothing.
fn keypad(usage: u16, num: bool) -> Option<Key> {
    Some(match usage {
        0x54 => Key::Char('/'),
        0x55 => Key::Char('*'),
        0x56 => Key::Char('-'),
        0x57 => Key::Char('+'),
        0x58 => Key::Named(NamedKey::Enter),
        0x59 => keypad_digit('1', NamedKey::End, num),
        0x5A => keypad_digit('2', NamedKey::Down, num),
        0x5B => keypad_digit('3', NamedKey::PageDown, num),
        0x5C => keypad_digit('4', NamedKey::Left, num),
        0x5D => {
            if num {
                Key::Char('5')
            } else {
                return None;
            }
        }
        0x5E => keypad_digit('6', NamedKey::Right, num),
        0x5F => keypad_digit('7', NamedKey::Home, num),
        0x60 => keypad_digit('8', NamedKey::Up, num),
        0x61 => keypad_digit('9', NamedKey::PageUp, num),
        0x62 => keypad_digit('0', NamedKey::Insert, num),
        0x63 => {
            if num {
                Key::Char('.')
            } else {
                Key::Named(NamedKey::Delete)
            }
        }
        _ => return None,
    })
}

/// A keypad key that is `digit` with Num Lock on and `nav` with it off.
fn keypad_digit(digit: char, nav: NamedKey, num: bool) -> Key {
    if num {
        Key::Char(digit)
    } else {
        Key::Named(nav)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What pressing and letting go of `key` typed.
    fn tap(keyboard: &mut KeyboardState, key: PhysicalKey) -> Option<Key> {
        let read = keyboard.edge(key, true).expect("a key not already down");
        keyboard.edge(key, false).expect("a key down");
        read.key
    }

    /// A pair of modifier keys and what of the held set it holds.
    type ModifierPair = (PhysicalKey, PhysicalKey, fn(Modifiers) -> bool);

    #[test]
    fn a_letter_takes_its_case_from_shift_or_caps_lock_but_not_both() {
        let mut keyboard = KeyboardState::new();
        assert_eq!(tap(&mut keyboard, PhysicalKey::A), Some(Key::Char('a')));
        assert_eq!(tap(&mut keyboard, PhysicalKey::CAPS_LOCK), None);
        assert_eq!(tap(&mut keyboard, PhysicalKey::A), Some(Key::Char('A')));
        assert!(keyboard.edge(PhysicalKey::LEFT_SHIFT, true).is_some());
        assert_eq!(tap(&mut keyboard, PhysicalKey::A), Some(Key::Char('a')));
    }

    #[test]
    fn shift_picks_the_symbol_on_a_digit_key_and_caps_lock_does_not() {
        let mut keyboard = KeyboardState::new();
        let _ = tap(&mut keyboard, PhysicalKey::CAPS_LOCK);
        let digits = [
            (PhysicalKey::DIGIT_3, '3', '#'),
            (PhysicalKey::DIGIT_4, '4', '$'),
            (PhysicalKey::DIGIT_5, '5', '%'),
        ];
        for (key, unshifted, _) in digits {
            assert_eq!(tap(&mut keyboard, key), Some(Key::Char(unshifted)));
        }
        let _ = keyboard.edge(PhysicalKey::RIGHT_SHIFT, true);
        for (key, _, shifted) in digits {
            assert_eq!(tap(&mut keyboard, key), Some(Key::Char(shifted)));
        }
    }

    #[test]
    fn every_edge_of_a_modifier_is_read_and_its_pair_holds_it_until_both_are_up() {
        let pairs: [ModifierPair; 4] = [
            (PhysicalKey::LEFT_SHIFT, PhysicalKey::RIGHT_SHIFT, |m| {
                m.shift
            }),
            (PhysicalKey::LEFT_CTRL, PhysicalKey::RIGHT_CTRL, |m| m.ctrl),
            (PhysicalKey::LEFT_ALT, PhysicalKey::RIGHT_ALT, |m| m.alt),
            (PhysicalKey::LEFT_GUI, PhysicalKey::RIGHT_GUI, |m| m.meta),
        ];
        for (left, right, holds) in pairs {
            let mut keyboard = KeyboardState::new();
            let down = keyboard.edge(left, true).expect("read");
            assert_eq!(down.key, None, "a modifier types nothing");
            assert!(holds(down.modifiers), "{left:?}");
            assert!(holds(
                keyboard
                    .edge(right, true)
                    .expect("read, its pair already held")
                    .modifiers
            ));
            assert!(
                holds(keyboard.edge(left, false).expect("read").modifiers),
                "{right:?} holds it"
            );
            let up = keyboard.edge(right, false).expect("read");
            assert_eq!(up.modifiers, Modifiers::default());
        }
    }

    #[test]
    fn a_keys_own_repeat_and_a_release_never_pressed_are_read_as_nothing() {
        let mut keyboard = KeyboardState::new();
        assert!(keyboard.edge(PhysicalKey::A, true).is_some());
        assert_eq!(keyboard.edge(PhysicalKey::A, true), None, "already down");
        assert!(keyboard.edge(PhysicalKey::A, false).is_some());
        assert_eq!(keyboard.edge(PhysicalKey::A, false), None, "already up");
        assert_eq!(
            keyboard.edge(PhysicalKey::SPACE, false),
            None,
            "never pressed"
        );
    }

    #[test]
    fn a_release_reads_as_its_press_did_whatever_changed_meanwhile() {
        let mut keyboard = KeyboardState::new();
        let keypad_up = PhysicalKey::from_usage(0x60).expect("keypad 8");
        let shift = keyboard.edge(PhysicalKey::LEFT_SHIFT, true);
        assert!(shift.is_some());
        let digit_down = keyboard.edge(PhysicalKey::DIGIT_3, true).expect("read");
        assert_eq!(digit_down.key, Some(Key::Char('#')));
        let letter_down = keyboard.edge(PhysicalKey::A, true).expect("read");
        assert_eq!(letter_down.key, Some(Key::Char('A')));
        let pad_down = keyboard.edge(keypad_up, true).expect("read");
        assert_eq!(pad_down.key, Some(Key::Named(NamedKey::Up)));
        assert!(keyboard.edge(PhysicalKey::LEFT_SHIFT, false).is_some());
        let _ = tap(&mut keyboard, PhysicalKey::CAPS_LOCK);
        let _ = tap(&mut keyboard, PhysicalKey::NUM_LOCK);
        for (key, typed) in [
            (PhysicalKey::DIGIT_3, Key::Char('#')),
            (PhysicalKey::A, Key::Char('A')),
            (keypad_up, Key::Named(NamedKey::Up)),
        ] {
            let up = keyboard.edge(key, false).expect("read");
            assert_eq!(up.key, Some(typed), "{key:?}");
            assert_eq!(up.modifiers, Modifiers::default());
        }
        // The next press reads under what holds now: caps and num on, no shift.
        assert_eq!(tap(&mut keyboard, PhysicalKey::A), Some(Key::Char('A')));
        assert_eq!(
            tap(&mut keyboard, PhysicalKey::DIGIT_3),
            Some(Key::Char('3'))
        );
        assert_eq!(tap(&mut keyboard, keypad_up), Some(Key::Char('8')));
    }

    #[test]
    fn a_lock_toggles_on_its_press_and_is_read_typing_nothing() {
        let mut keyboard = KeyboardState::new();
        for _ in 0..2 {
            let press = keyboard.edge(PhysicalKey::CAPS_LOCK, true).expect("read");
            assert_eq!(press.key, None);
            assert_eq!(
                keyboard
                    .edge(PhysicalKey::CAPS_LOCK, false)
                    .map(|read| read.key),
                Some(None)
            );
        }
        assert_eq!(
            tap(&mut keyboard, PhysicalKey::A),
            Some(Key::Char('a')),
            "toggled twice"
        );
    }

    #[test]
    fn num_lock_turns_the_keypad_from_navigation_to_digits() {
        let keypad_end = PhysicalKey::from_usage(0x59).expect("a key");
        let keypad_five = PhysicalKey::from_usage(0x5D).expect("a key");
        let mut keyboard = KeyboardState::new();
        assert_eq!(
            tap(&mut keyboard, keypad_end),
            Some(Key::Named(NamedKey::End))
        );
        assert_eq!(tap(&mut keyboard, keypad_five), None);
        let _ = tap(&mut keyboard, PhysicalKey::NUM_LOCK);
        assert_eq!(tap(&mut keyboard, keypad_end), Some(Key::Char('1')));
        assert_eq!(tap(&mut keyboard, keypad_five), Some(Key::Char('5')));
    }

    #[test]
    fn the_named_keys_resolve_and_a_key_the_layout_leaves_unnamed_types_nothing() {
        let mut keyboard = KeyboardState::new();
        assert_eq!(
            tap(&mut keyboard, PhysicalKey::ESCAPE),
            Some(Key::Named(NamedKey::Escape))
        );
        assert_eq!(
            tap(&mut keyboard, PhysicalKey::from_usage(0x3A).expect("a key")),
            Some(Key::Named(NamedKey::Function { number: 1 }))
        );
        assert_eq!(
            tap(&mut keyboard, PhysicalKey::from_usage(0x45).expect("a key")),
            Some(Key::Named(NamedKey::Function { number: 12 }))
        );
        assert_eq!(tap(&mut keyboard, PhysicalKey::SPACE), Some(Key::Char(' ')));
        // Print Screen is read, typing nothing.
        let print = keyboard
            .edge(PhysicalKey::from_usage(0x46).expect("a key"), true)
            .expect("read");
        assert_eq!(print.key, None);
    }
}
