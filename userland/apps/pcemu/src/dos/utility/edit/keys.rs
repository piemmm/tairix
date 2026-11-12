//! The keys `EDIT` reads: a code from INT 16h, the shift flags read beside
//! it, and the codes of its own the keyboard and mouse hooks put in the
//! BIOS's buffer. On the 83-key keyboard the arrows are the keypad's, and
//! Shift turns them into digits, so a keypad digit typed with Shift held
//! and Num Lock off is the move it stands on, Shift with it.

use crate::bios::{on_keypad, shift};
use crate::dos::char::scan;

/// The scan codes of the editor's own keys, which no key's code has.
pub mod own {
    /// Alt went down.
    pub const ALT_DOWN: u8 = 0xF0;
    /// Alt came up.
    pub const ALT_UP: u8 = 0xF1;
    /// The mouse did something.
    pub const MOUSE: u8 = 0xF2;
    /// Ctrl+Ins, which an 83-key keyboard's BIOS gives no code.
    pub const COPY: u8 = 0xF3;
}

/// A key as the editor takes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Press {
    /// Its character, none for a key that types none.
    pub char: Option<u8>,
    /// Its scan code.
    pub scan: u8,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Press {
    /// The key INT 16h read as `code` with the shift flags `flags`.
    #[must_use]
    pub fn of(code: u16, flags: u8) -> Self {
        let [ascii, scan] = code.to_le_bytes();
        let shifted = flags & (shift::LEFT_SHIFT | shift::RIGHT_SHIFT) != 0;
        let num = flags & shift::NUM != 0;
        let ctrl = flags & shift::CTRL != 0;
        let alt = flags & shift::ALT != 0;
        let keypad_digit = on_keypad(scan) && (ascii.is_ascii_digit() || ascii == b'.');
        let (char, shift) = match ascii {
            _ if keypad_digit && shifted && !num => (None, true),
            0 | 0xE0 if on_keypad(scan) && num => (None, false),
            0 | 0xE0 => (None, shifted),
            _ => (Some(ascii), shifted),
        };
        Self {
            char,
            scan,
            shift,
            ctrl,
            alt,
        }
    }

    /// Whether it is the key `scan` with no character.
    #[must_use]
    pub fn is(self, scan: u8) -> bool {
        self.char.is_none() && self.scan == scan
    }

    /// The letter or digit a Ctrl or Alt combination was made with.
    #[must_use]
    pub fn letter(self) -> Option<u8> {
        match self.char {
            Some(control @ 1..=26)
                if self.scan != scan::ENTER
                    && self.scan != scan::TAB
                    && self.scan != scan::BACKSPACE =>
            {
                Some(b'A' + control - 1)
            }
            Some(byte) if byte.is_ascii_alphanumeric() => Some(byte.to_ascii_uppercase()),
            _ => letter_of(self.scan),
        }
    }
}

/// The letter whose key gives scan code `scan`, as Alt with it does.
#[must_use]
pub fn letter_of(scan: u8) -> Option<u8> {
    const ROWS: [(u8, &[u8]); 3] = [
        (0x10, b"QWERTYUIOP"),
        (0x1E, b"ASDFGHJKL"),
        (0x2C, b"ZXCVBNM"),
    ];
    ROWS.iter()
        .find_map(|&(first, letters)| letters.get(usize::from(scan.checked_sub(first)?)).copied())
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
