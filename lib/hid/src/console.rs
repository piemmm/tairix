//! The keyboard's console producer: a key edge, named by its HID usage,
//! resolved through the held modifiers and the caps and num locks into the
//! [`Key`] a US layout produces, and emitted as the [`KeyInput`] record the
//! seat routes (`plans/PI.md` P11).
//!
//! The usage-to-[`Key`] table is HID's own; the [`Key`]-to-[`KeyInput`] map is
//! `lib/keymap`'s. An unknown usage or a non-key event produces nothing.

use tairix_abi::driver::input::{InputEvent, InputEventKind};
use tairix_abi::input::KeyInput;
use tairix_input::{Key, ModifierKey, ModifierSide, ModifierState, NamedKey};
use tairix_keymap::{key_input, modifier_change};

use crate::usages::MODIFIER_FIRST;

/// HID usage of the Caps Lock key (HID Usage Tables, page `0x07`).
const USAGE_CAPS_LOCK: u16 = 0x39;

/// HID usage of the Num Lock / Clear key.
const USAGE_NUM_LOCK: u16 = 0x53;

/// The modifier a usage names, or [`None`] if it is not one of the eight
/// keyboard modifiers (HID Usage Tables, page `0x07`).
const fn modifier_of(usage: u16) -> Option<(ModifierKey, ModifierSide)> {
    let Some(offset) = usage.checked_sub(MODIFIER_FIRST) else {
        return None;
    };
    Some(match offset {
        0 => (ModifierKey::Ctrl, ModifierSide::Left),
        1 => (ModifierKey::Shift, ModifierSide::Left),
        2 => (ModifierKey::Alt, ModifierSide::Left),
        3 => (ModifierKey::Meta, ModifierSide::Left),
        4 => (ModifierKey::Ctrl, ModifierSide::Right),
        5 => (ModifierKey::Shift, ModifierSide::Right),
        6 => (ModifierKey::Alt, ModifierSide::Right),
        7 => (ModifierKey::Meta, ModifierSide::Right),
        _ => return None,
    })
}

/// `value` of a press edge in an [`InputEvent`].
const VALUE_PRESS: i32 = 1;
/// `value` of a release edge.
const VALUE_RELEASE: i32 = 0;

/// Key edges, each an [`InputEvent`] naming a HID usage, resolved into
/// [`KeyInput`] records.
///
/// The only state carried between [`feed`](Self::feed) calls is the held
/// modifiers and the two lock toggles.
#[derive(Debug, Default)]
pub struct KeyboardConsole {
    modifiers: ModifierState,
    caps_lock: bool,
    num_lock: bool,
}

impl KeyboardConsole {
    /// A producer with no keys held and both locks off.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            modifiers: ModifierState::new(),
            caps_lock: false,
            num_lock: false,
        }
    }

    /// Feed one decoded keyboard [`InputEvent`], returning the
    /// [`KeyInput`] record its key edge resolves to, or [`None`] when the
    /// edge produces no record.
    ///
    /// A modifier edge that changes the *observable* set of held modifiers
    /// produces a [`KeyInput::ModifiersChanged`] record — the desktop needs it
    /// to qualify a gesture that is not a key (a shift-click) — while one that
    /// does not (a repeat, or letting go of one shift key while the other is
    /// held) produces nothing. The lock keys update the internal state and
    /// produce no record. A *press* or *release* of a printable or named
    /// key produces the corresponding [`KeyInput::Pressed`] /
    /// [`KeyInput::Released`] record carrying the resolved [`Key`] and the
    /// modifiers held; an unknown usage or a non-keyboard event produces
    /// nothing. Both edges are emitted so the desktop sees key-up as well
    /// as key-down; the text path ignores releases in the kernel arbiter.
    ///
    /// # Capabilities
    ///
    /// None (the producer holds no authority; delivery is the sink's).
    pub fn feed(&mut self, event: InputEvent) -> Option<KeyInput> {
        if event.kind != InputEventKind::Key {
            return None;
        }
        let pressed = match event.value {
            VALUE_PRESS => true,
            VALUE_RELEASE => false,
            _ => return None,
        };
        let usage = event.code;
        if let Some((key, side)) = modifier_of(usage) {
            let changed = if pressed {
                self.modifiers.press(key, side)
            } else {
                self.modifiers.release(key, side)
            };
            return changed.then(|| modifier_change(self.modifiers.modifiers()));
        }
        if usage == USAGE_CAPS_LOCK {
            if pressed {
                self.caps_lock = !self.caps_lock;
            }
            return None;
        }
        if usage == USAGE_NUM_LOCK {
            if pressed {
                self.num_lock = !self.num_lock;
            }
            return None;
        }
        let modifiers = self.modifiers.modifiers();
        let key = resolve_usage(usage, modifiers.shift, self.caps_lock, self.num_lock)?;
        // `key_input` returns `None` only for a `Key` with no wire form (a
        // function number outside `F1..=F12`); `resolve_usage` never
        // produces one, so a resolvable key always yields a record.
        key_input(key, modifiers, pressed)
    }
}

/// Resolve a HID page-`0x07` usage to the [`Key`] a US keyboard layout
/// produces, given the active `shift`, `caps`, and `num` lock state.
///
/// Returns `None` for a usage with no console key (an unmapped usage, a lock
/// key, or numeric-keypad `5` with Num Lock off) — fail closed, never guess.
fn resolve_usage(usage: u16, shift: bool, caps: bool, num: bool) -> Option<Key> {
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

/// Map a letter usage (`0x04`=`A`..`0x1D`=`Z`) to its character, applying
/// shift XOR caps lock for the case.
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

/// Map a printable non-letter usage (digits, space, punctuation) to its
/// character, selecting the shifted glyph when `shift` is held.
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

/// Map a named-key usage (editing, navigation, function, the main-block
/// control keys) to its [`NamedKey`].
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

/// Map a numeric-keypad usage to its [`Key`], honouring Num Lock.
///
/// With Num Lock on the keypad sends digits, `.`, and the operators; with it
/// off the digit keys are the navigation cluster (`KP1`=End, `KP8`=Up, …) and
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
mod tests;
