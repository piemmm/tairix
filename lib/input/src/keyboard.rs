//! What every keyboard producer shares once it has named a key edge's
//! [`PhysicalKey`]: the held modifiers, the caps and num locks, and the US
//! layout.
//!
//! A producer maps its device's codes onto the physical key — a HID usage is
//! one already, an `evdev` keycode goes through its driver's one table — and
//! feeds each edge to [`KeyboardState::edge`], so two drivers cannot resolve
//! the same key differently.

use tairix_abi::input::PhysicalKey;

use crate::{Key, ModifierKey, ModifierSide, ModifierState, Modifiers, NamedKey};

/// What one key edge produced.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum KeyEdge {
    /// The held modifiers changed to these.
    Modifiers(Modifiers),
    /// A key went down or came up, producing this under the modifiers held.
    Key(Key),
}

/// A keyboard as its key edges have left it: the modifier keys held, and the
/// caps and num locks, both off until first pressed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct KeyboardState {
    modifiers: ModifierState,
    caps_lock: bool,
    num_lock: bool,
}

impl KeyboardState {
    /// A keyboard with no key held and both locks off.
    #[must_use]
    pub const fn new() -> Self {
        Self {
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

    /// Apply one edge of `key`, a press when `pressed`, answering what it
    /// produced.
    ///
    /// A modifier key answers [`KeyEdge::Modifiers`] only when the held set
    /// changed: a repeat, or letting go of one shift key while the other is
    /// held, produces nothing. Caps Lock and Num Lock toggle on their press and
    /// produce nothing. Any other key answers what the US layout makes of it
    /// under the shift key and the locks, or nothing where the layout names
    /// none: an unknown key, or keypad 5 with Num Lock off.
    pub fn edge(&mut self, key: PhysicalKey, pressed: bool) -> Option<KeyEdge> {
        if let Some((modifier, side)) = modifier_of(key) {
            let changed = if pressed {
                self.modifiers.press(modifier, side)
            } else {
                self.modifiers.release(modifier, side)
            };
            return changed.then(|| KeyEdge::Modifiers(self.modifiers()));
        }
        if key == PhysicalKey::CAPS_LOCK {
            if pressed {
                self.caps_lock = !self.caps_lock;
            }
            return None;
        }
        if key == PhysicalKey::NUM_LOCK {
            if pressed {
                self.num_lock = !self.num_lock;
            }
            return None;
        }
        let shift = self.modifiers().shift;
        us_layout(key.usage(), shift, self.caps_lock, self.num_lock).map(KeyEdge::Key)
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

    /// What pressing `key` produced.
    fn press(keyboard: &mut KeyboardState, key: PhysicalKey) -> Option<KeyEdge> {
        keyboard.edge(key, true)
    }

    /// The edge of a key typing `ch`.
    fn typed(ch: char) -> KeyEdge {
        KeyEdge::Key(Key::Char(ch))
    }

    /// The edge of a key naming `named`.
    fn named_edge(named: NamedKey) -> KeyEdge {
        KeyEdge::Key(Key::Named(named))
    }

    /// A pair of modifier keys and what of the held set it holds.
    type ModifierPair = (PhysicalKey, PhysicalKey, fn(Modifiers) -> bool);

    #[test]
    fn a_letter_takes_its_case_from_shift_or_caps_lock_but_not_both() {
        let mut keyboard = KeyboardState::new();
        assert_eq!(press(&mut keyboard, PhysicalKey::A), Some(typed('a')));
        assert_eq!(press(&mut keyboard, PhysicalKey::CAPS_LOCK), None);
        assert_eq!(press(&mut keyboard, PhysicalKey::A), Some(typed('A')));
        assert!(press(&mut keyboard, PhysicalKey::LEFT_SHIFT).is_some());
        assert_eq!(press(&mut keyboard, PhysicalKey::A), Some(typed('a')));
        assert_eq!(keyboard.edge(PhysicalKey::A, false), Some(typed('a')));
    }

    #[test]
    fn shift_picks_the_symbol_on_a_digit_key_and_caps_lock_does_not() {
        let mut keyboard = KeyboardState::new();
        let _ = press(&mut keyboard, PhysicalKey::CAPS_LOCK);
        let digits = [
            (PhysicalKey::DIGIT_3, '3', '#'),
            (PhysicalKey::DIGIT_4, '4', '$'),
            (PhysicalKey::DIGIT_5, '5', '%'),
        ];
        for (key, unshifted, _) in digits {
            assert_eq!(press(&mut keyboard, key), Some(typed(unshifted)));
        }
        let _ = press(&mut keyboard, PhysicalKey::RIGHT_SHIFT);
        for (key, _, shifted) in digits {
            assert_eq!(press(&mut keyboard, key), Some(typed(shifted)));
        }
    }

    #[test]
    fn a_modifier_key_holds_its_modifier_until_both_of_its_pair_are_up() {
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
            let Some(KeyEdge::Modifiers(down)) = press(&mut keyboard, left) else {
                panic!("{left:?} is a modifier");
            };
            assert!(holds(down), "{left:?}");
            assert_eq!(press(&mut keyboard, right), None, "already held");
            assert_eq!(keyboard.edge(left, false), None, "{right:?} holds it");
            assert!(holds(keyboard.modifiers()));
            assert_eq!(
                keyboard.edge(right, false),
                Some(KeyEdge::Modifiers(Modifiers::default()))
            );
        }
    }

    #[test]
    fn a_lock_toggles_on_its_press_alone() {
        let mut keyboard = KeyboardState::new();
        for _ in 0..2 {
            assert_eq!(press(&mut keyboard, PhysicalKey::CAPS_LOCK), None);
            assert_eq!(keyboard.edge(PhysicalKey::CAPS_LOCK, false), None);
        }
        assert_eq!(press(&mut keyboard, PhysicalKey::A), Some(typed('a')));
    }

    #[test]
    fn num_lock_turns_the_keypad_from_navigation_to_digits() {
        let keypad_end = PhysicalKey::from_usage(0x59);
        let keypad_five = PhysicalKey::from_usage(0x5D);
        let mut keyboard = KeyboardState::new();
        assert_eq!(
            press(&mut keyboard, keypad_end),
            Some(named_edge(NamedKey::End))
        );
        assert_eq!(press(&mut keyboard, keypad_five), None);
        assert_eq!(press(&mut keyboard, PhysicalKey::NUM_LOCK), None);
        assert_eq!(press(&mut keyboard, keypad_end), Some(typed('1')));
        assert_eq!(press(&mut keyboard, keypad_five), Some(typed('5')));
    }

    #[test]
    fn the_named_keys_resolve_and_a_key_the_layout_leaves_unnamed_produces_nothing() {
        let mut keyboard = KeyboardState::new();
        assert_eq!(
            press(&mut keyboard, PhysicalKey::ESCAPE),
            Some(named_edge(NamedKey::Escape))
        );
        assert_eq!(
            press(&mut keyboard, PhysicalKey::from_usage(0x3A)),
            Some(named_edge(NamedKey::Function { number: 1 }))
        );
        assert_eq!(
            press(&mut keyboard, PhysicalKey::from_usage(0x45)),
            Some(named_edge(NamedKey::Function { number: 12 }))
        );
        assert_eq!(press(&mut keyboard, PhysicalKey::SPACE), Some(typed(' ')));
        assert_eq!(press(&mut keyboard, PhysicalKey::UNKNOWN), None);
        // Print Screen is a key this layout types nothing for.
        assert_eq!(press(&mut keyboard, PhysicalKey::from_usage(0x46)), None);
    }
}
