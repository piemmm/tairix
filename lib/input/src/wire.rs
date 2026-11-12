//! The one crossing between the wire keyboard vocabulary
//! (`tairix_abi::input`) and this crate's: a kernel arbiter, the desktop seat
//! and an application's own key handling all read a record through these, so
//! no two of them can disagree about which key or modifier it names.

use tairix_abi::input::{
    KeyInput, KeyValue, Modifiers as WireModifiers, NamedKeyCode, PointerButtonCode,
};

use crate::{InputEvent, Key, Modifiers, NamedKey, PointerButton};

/// The [`InputEvent`] one delivered [`KeyInput`] record means. Total: every
/// key has exactly one meaning, so no reader decides what an unhandled code
/// means. A key that typed nothing — a modifier, a lock, a key the layout
/// names no value for — means only the modifiers it left held.
#[must_use]
pub const fn key_input_event(record: KeyInput) -> InputEvent {
    let modifiers = modifiers_from_abi(record.modifiers());
    match (record, record.key()) {
        (_, None) => InputEvent::ModifiersChanged { modifiers },
        (KeyInput::Pressed { .. }, Some(key)) => InputEvent::KeyPressed {
            key: key_from_abi(key),
            modifiers,
        },
        (KeyInput::Released { .. }, Some(key)) => InputEvent::KeyReleased {
            key: key_from_abi(key),
            modifiers,
        },
    }
}

/// The [`InputEvent`] telling a surface the modifiers `held`, which a pointer
/// event the desktop delivered was made under: what it reads a shift-click by.
#[must_use]
pub const fn held_modifiers(held: WireModifiers) -> InputEvent {
    InputEvent::ModifiersChanged {
        modifiers: modifiers_from_abi(held),
    }
}

/// The [`Key`] a wire [`KeyValue`] names.
#[must_use]
pub const fn key_from_abi(value: KeyValue) -> Key {
    match value {
        KeyValue::Char(c) => Key::Char(c),
        KeyValue::Named(code) => Key::Named(named_key_from_abi(code)),
    }
}

/// The [`NamedKey`] a wire [`NamedKeyCode`] names: the wire gives each
/// function key its own code, this crate one [`NamedKey::Function`] carrying
/// the number.
#[must_use]
pub const fn named_key_from_abi(code: NamedKeyCode) -> NamedKey {
    match code {
        NamedKeyCode::Enter => NamedKey::Enter,
        NamedKeyCode::Escape => NamedKey::Escape,
        NamedKeyCode::Backspace => NamedKey::Backspace,
        NamedKeyCode::Tab => NamedKey::Tab,
        NamedKeyCode::Delete => NamedKey::Delete,
        NamedKeyCode::Insert => NamedKey::Insert,
        NamedKeyCode::Home => NamedKey::Home,
        NamedKeyCode::End => NamedKey::End,
        NamedKeyCode::PageUp => NamedKey::PageUp,
        NamedKeyCode::PageDown => NamedKey::PageDown,
        NamedKeyCode::Left => NamedKey::Left,
        NamedKeyCode::Right => NamedKey::Right,
        NamedKeyCode::Up => NamedKey::Up,
        NamedKeyCode::Down => NamedKey::Down,
        NamedKeyCode::F1 => NamedKey::Function { number: 1 },
        NamedKeyCode::F2 => NamedKey::Function { number: 2 },
        NamedKeyCode::F3 => NamedKey::Function { number: 3 },
        NamedKeyCode::F4 => NamedKey::Function { number: 4 },
        NamedKeyCode::F5 => NamedKey::Function { number: 5 },
        NamedKeyCode::F6 => NamedKey::Function { number: 6 },
        NamedKeyCode::F7 => NamedKey::Function { number: 7 },
        NamedKeyCode::F8 => NamedKey::Function { number: 8 },
        NamedKeyCode::F9 => NamedKey::Function { number: 9 },
        NamedKeyCode::F10 => NamedKey::Function { number: 10 },
        NamedKeyCode::F11 => NamedKey::Function { number: 11 },
        NamedKeyCode::F12 => NamedKey::Function { number: 12 },
    }
}

/// The wire [`NamedKeyCode`] of `named`, or `None` for a function number the
/// wire does not carry (outside `F1..=F12`).
#[must_use]
pub const fn named_key_to_abi(named: NamedKey) -> Option<NamedKeyCode> {
    Some(match named {
        NamedKey::Enter => NamedKeyCode::Enter,
        NamedKey::Escape => NamedKeyCode::Escape,
        NamedKey::Backspace => NamedKeyCode::Backspace,
        NamedKey::Tab => NamedKeyCode::Tab,
        NamedKey::Delete => NamedKeyCode::Delete,
        NamedKey::Insert => NamedKeyCode::Insert,
        NamedKey::Home => NamedKeyCode::Home,
        NamedKey::End => NamedKeyCode::End,
        NamedKey::PageUp => NamedKeyCode::PageUp,
        NamedKey::PageDown => NamedKeyCode::PageDown,
        NamedKey::Left => NamedKeyCode::Left,
        NamedKey::Right => NamedKeyCode::Right,
        NamedKey::Up => NamedKeyCode::Up,
        NamedKey::Down => NamedKeyCode::Down,
        NamedKey::Function { number } => match number {
            1 => NamedKeyCode::F1,
            2 => NamedKeyCode::F2,
            3 => NamedKeyCode::F3,
            4 => NamedKeyCode::F4,
            5 => NamedKeyCode::F5,
            6 => NamedKeyCode::F6,
            7 => NamedKeyCode::F7,
            8 => NamedKeyCode::F8,
            9 => NamedKeyCode::F9,
            10 => NamedKeyCode::F10,
            11 => NamedKeyCode::F11,
            12 => NamedKeyCode::F12,
            _ => return None,
        },
    })
}

/// The [`PointerButton`] a wire [`PointerButtonCode`] names.
#[must_use]
pub const fn pointer_button_from_abi(code: PointerButtonCode) -> PointerButton {
    match code {
        PointerButtonCode::Primary => PointerButton::Primary,
        PointerButtonCode::Secondary => PointerButton::Secondary,
        PointerButtonCode::Middle => PointerButton::Middle,
    }
}

/// The wire [`PointerButtonCode`] of `button`, the inverse of
/// [`pointer_button_from_abi`].
#[must_use]
pub const fn pointer_button_to_abi(button: PointerButton) -> PointerButtonCode {
    match button {
        PointerButton::Primary => PointerButtonCode::Primary,
        PointerButton::Secondary => PointerButtonCode::Secondary,
        PointerButton::Middle => PointerButtonCode::Middle,
    }
}

/// The [`Modifiers`] a wire modifier set names.
#[must_use]
pub const fn modifiers_from_abi(m: WireModifiers) -> Modifiers {
    Modifiers {
        shift: m.shift,
        ctrl: m.ctrl,
        alt: m.alt,
        meta: m.meta,
    }
}

/// The wire modifier set `m` is, the inverse of [`modifiers_from_abi`]: the
/// desktop stamps the seat's modifiers onto the wire events it delivers.
#[must_use]
pub const fn modifiers_to_abi(m: Modifiers) -> WireModifiers {
    WireModifiers {
        shift: m.shift,
        ctrl: m.ctrl,
        alt: m.alt,
        meta: m.meta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every wire code names a key that maps back to it, and the wire carries
    /// no function key past F12.
    #[test]
    fn every_named_key_crosses_both_ways() {
        let mut code = 1;
        while let Ok(named) = NamedKeyCode::from_code(code) {
            assert_eq!(named_key_to_abi(named_key_from_abi(named)), Some(named));
            code += 1;
        }
        assert_eq!(code, 27, "every wire code was crossed");
        for number in [0, 13, u8::MAX] {
            assert_eq!(named_key_to_abi(NamedKey::Function { number }), None);
        }
    }

    /// A record reads as the event its kind names, with its key and modifiers.
    #[test]
    fn a_record_reads_as_its_event() {
        let shift = tairix_abi::input::Modifiers {
            shift: true,
            ..tairix_abi::input::Modifiers::default()
        };
        let physical = tairix_abi::input::PhysicalKey::A;
        let key = Some(KeyValue::Char('A'));
        let read = |record| key_input_event(record);
        let held = modifiers_from_abi(shift);
        assert_eq!(
            read(KeyInput::Pressed {
                key,
                modifiers: shift,
                physical,
            }),
            InputEvent::KeyPressed {
                key: Key::Char('A'),
                modifiers: held,
            }
        );
        assert_eq!(
            read(KeyInput::Released {
                key,
                modifiers: shift,
                physical,
            }),
            InputEvent::KeyReleased {
                key: Key::Char('A'),
                modifiers: held,
            }
        );
        assert_eq!(
            read(KeyInput::Pressed {
                key: None,
                modifiers: shift,
                physical: tairix_abi::input::PhysicalKey::LEFT_SHIFT,
            }),
            InputEvent::ModifiersChanged { modifiers: held },
            "a key that types nothing tells only the modifiers it left held"
        );
    }

    /// A character and a named key read as themselves.
    #[test]
    fn a_key_value_reads_as_its_key() {
        assert_eq!(key_from_abi(KeyValue::Char('é')), Key::Char('é'));
        assert_eq!(
            key_from_abi(KeyValue::Named(NamedKeyCode::F7)),
            Key::Named(NamedKey::Function { number: 7 })
        );
    }

    /// Each button crosses both ways.
    #[test]
    fn each_pointer_button_crosses_both_ways() {
        for code in [
            PointerButtonCode::Primary,
            PointerButtonCode::Secondary,
            PointerButtonCode::Middle,
        ] {
            assert_eq!(pointer_button_to_abi(pointer_button_from_abi(code)), code);
        }
    }

    /// Each modifier crosses both ways on its own.
    #[test]
    fn each_modifier_crosses_both_ways() {
        let one = |shift, ctrl, alt, meta| Modifiers {
            shift,
            ctrl,
            alt,
            meta,
        };
        for m in [
            Modifiers::default(),
            one(true, false, false, false),
            one(false, true, false, false),
            one(false, false, true, false),
            one(false, false, false, true),
        ] {
            assert_eq!(modifiers_from_abi(modifiers_to_abi(m)), m);
        }
    }
}
