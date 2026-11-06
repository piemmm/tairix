//! The capture chords, matched by where a key sits on the keyboard rather
//! than by what the layout makes of it.

use tairix_abi::input::{KeyInput, KeyValue, NamedKeyCode, PhysicalKey};

/// What a capture chord asks for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ChordAsk {
    /// Meta+Shift+3: the whole screen, at once.
    Screen,
    /// Meta+Shift+4: the area overlay, where Space picks a window.
    Area,
    /// Meta+Shift+5: the capture bar.
    Bar,
}

/// A capture chord: what it asks for, and whether Ctrl puts the still on the
/// clipboard.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CaptureChord {
    /// What it asks for.
    pub ask: ChordAsk,
    /// Whether Ctrl was held.
    pub clipboard: bool,
}

impl CaptureChord {
    /// The chord `key` presses, or `None`. Meta stands for Command; Alt held
    /// makes no chord, so the shortcut never shadows an application's own.
    #[must_use]
    pub fn of(key: &KeyInput) -> Option<Self> {
        let KeyInput::Pressed {
            modifiers,
            physical,
            ..
        } = *key
        else {
            return None;
        };
        if !modifiers.meta || !modifiers.shift || modifiers.alt {
            return None;
        }
        let ask = match physical {
            PhysicalKey::DIGIT_3 => ChordAsk::Screen,
            PhysicalKey::DIGIT_4 => ChordAsk::Area,
            PhysicalKey::DIGIT_5 => ChordAsk::Bar,
            _ => return None,
        };
        Some(Self {
            ask,
            clipboard: modifiers.ctrl,
        })
    }
}

/// Whether `key` presses Escape.
#[must_use]
pub fn is_escape(key: &KeyInput) -> bool {
    matches!(
        key,
        KeyInput::Pressed {
            physical: PhysicalKey::ESCAPE,
            ..
        } | KeyInput::Pressed {
            key: KeyValue::Named(NamedKeyCode::Escape),
            ..
        }
    )
}

/// Whether `key` presses Space.
#[must_use]
pub fn is_space(key: &KeyInput) -> bool {
    matches!(
        key,
        KeyInput::Pressed {
            physical: PhysicalKey::SPACE,
            ..
        } | KeyInput::Pressed {
            key: KeyValue::Char(' '),
            ..
        }
    )
}
