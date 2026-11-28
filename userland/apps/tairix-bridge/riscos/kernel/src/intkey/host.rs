//! A host's keys as the keyboard's: each USB HID usage on the
//! Keyboard/Keypad page as the low-level key number a PC keyboard gives the
//! key in its place, which `KeyV` and the key handler take.

use tairix_abi::input::PhysicalKey;

/// No low-level key: a usage no PC keyboard key stands in.
const NONE: u8 = 0xFF;

/// Each usage a PC keyboard has a key for, and that key's low-level number.
/// The ANSI backslash and the ISO hash share one key's place, as PS/2 gives
/// them one scan code.
const PLACES: [(u16, u8); 104] = [
    (0x04, 0x3C), // A
    (0x05, 0x52),
    (0x06, 0x50),
    (0x07, 0x3E),
    (0x08, 0x29),
    (0x09, 0x3F),
    (0x0A, 0x40),
    (0x0B, 0x41),
    (0x0C, 0x2E),
    (0x0D, 0x42),
    (0x0E, 0x43),
    (0x0F, 0x44),
    (0x10, 0x54),
    (0x11, 0x53),
    (0x12, 0x2F),
    (0x13, 0x30),
    (0x14, 0x27),
    (0x15, 0x2A),
    (0x16, 0x3D),
    (0x17, 0x2B),
    (0x18, 0x2D),
    (0x19, 0x51),
    (0x1A, 0x28),
    (0x1B, 0x4F),
    (0x1C, 0x2C),
    (0x1D, 0x4E), // Z
    (0x1E, 0x11), // 1
    (0x1F, 0x12),
    (0x20, 0x13),
    (0x21, 0x14),
    (0x22, 0x15),
    (0x23, 0x16),
    (0x24, 0x17),
    (0x25, 0x18),
    (0x26, 0x19),
    (0x27, 0x1A), // 0
    (0x28, 0x47), // Return
    (0x29, 0x00), // Escape
    (0x2A, 0x1E), // Backspace
    (0x2B, 0x26), // Tab
    (0x2C, 0x5F), // Space
    (0x2D, 0x1B), // -
    (0x2E, 0x1C), // =
    (0x2F, 0x31), // [
    (0x30, 0x32), // ]
    (0x31, 0x33), // \ on ANSI keyboards
    (0x32, 0x33), // # on ISO keyboards
    (0x33, 0x45), // ;
    (0x34, 0x46), // '
    (0x35, 0x10), // `
    (0x36, 0x55), // ,
    (0x37, 0x56), // .
    (0x38, 0x57), // /
    (0x39, 0x5D), // Caps Lock
    (0x3A, 0x01), // F1
    (0x3B, 0x02),
    (0x3C, 0x03),
    (0x3D, 0x04),
    (0x3E, 0x05),
    (0x3F, 0x06),
    (0x40, 0x07),
    (0x41, 0x08),
    (0x42, 0x09),
    (0x43, 0x0A),
    (0x44, 0x0B),
    (0x45, 0x0C), // F12
    (0x46, 0x0D), // Print
    (0x47, 0x0E), // Scroll Lock
    (0x48, 0x0F), // Pause, which is Break
    (0x49, 0x1F), // Insert
    (0x4A, 0x20), // Home
    (0x4B, 0x21), // Page Up
    (0x4C, 0x34), // Delete
    (0x4D, 0x35), // End, which is Copy
    (0x4E, 0x36), // Page Down
    (0x4F, 0x64), // Right
    (0x50, 0x62), // Left
    (0x51, 0x63), // Down
    (0x52, 0x59), // Up
    (0x53, 0x22), // Num Lock
    (0x54, 0x23), // keypad /
    (0x55, 0x24), // keypad *
    (0x56, 0x3A), // keypad -
    (0x57, 0x4B), // keypad +
    (0x58, 0x67), // keypad Enter
    (0x59, 0x5A), // keypad 1
    (0x5A, 0x5B),
    (0x5B, 0x5C),
    (0x5C, 0x48),
    (0x5D, 0x49),
    (0x5E, 0x4A),
    (0x5F, 0x37),
    (0x60, 0x38),
    (0x61, 0x39), // keypad 9
    (0x62, 0x65), // keypad 0
    (0x63, 0x66), // keypad .
    (0x64, 0x4D), // \ beside the left Shift on ISO keyboards
    (0x65, 0x6A), // Menu
    (0x87, 0x6E), // Ro
    (0x88, 0x6D), // Kana
    (0x89, 0x1D), // Yen
    (0x8A, 0x6C), // Convert
    (0x8B, 0x6B), // No Convert
    (0xCC, 0x25), // keypad #
];

/// The modifiers' places, which follow the page's last run.
const MODIFIERS: [(u16, u8); 8] = [
    (0xE0, 0x3B), // left Ctrl
    (0xE1, 0x4C), // left Shift
    (0xE2, 0x5E), // left Alt
    (0xE3, 0x68), // left logo
    (0xE4, 0x61), // right Ctrl
    (0xE5, 0x58), // right Shift
    (0xE6, 0x60), // right Alt
    (0xE7, 0x69), // right logo
];

/// One past the highest usage with a place.
const USAGES: usize = 0xE8;

/// [`PLACES`] and [`MODIFIERS`] by usage.
const BY_USAGE: [u8; USAGES] = {
    let mut table = [NONE; USAGES];
    let mut at = 0;
    while at < PLACES.len() {
        table[PLACES[at].0 as usize] = PLACES[at].1;
        at += 1;
    }
    let mut at = 0;
    while at < MODIFIERS.len() {
        table[MODIFIERS[at].0 as usize] = MODIFIERS[at].1;
        at += 1;
    }
    table
};

/// The low-level key number of the key in `key`'s place on a PC keyboard:
/// none for a key no PC keyboard has.
#[must_use]
pub fn low_level(key: PhysicalKey) -> Option<u32> {
    BY_USAGE
        .get(usize::from(key.usage()))
        .copied()
        .filter(|&number| number != NONE)
        .map(u32::from)
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    fn usages() -> impl Iterator<Item = PhysicalKey> {
        (0..=u16::MAX).filter_map(PhysicalKey::from_usage)
    }

    #[test]
    fn every_place_is_a_key_the_handler_numbers_internally() {
        for key in usages() {
            if let Some(number) = low_level(key) {
                assert!(
                    (number as usize) < super::super::INTERNAL.len(),
                    "{:#X} gives &{number:X}",
                    key.usage()
                );
            }
        }
    }

    #[test]
    fn no_two_keys_share_a_place_but_the_backslash_and_the_hash() {
        let mut taken = [None; 0x80];
        for key in usages() {
            let Some(number) = low_level(key) else {
                continue;
            };
            if let Some(other) = taken[number as usize].replace(key.usage()) {
                assert_eq!(
                    (other, key.usage()),
                    (0x31, 0x32),
                    "&{number:X} given twice"
                );
            }
        }
    }

    #[test]
    fn the_keys_a_layout_names_are_where_its_rows_put_them() {
        let row = |keys: &[u16]| -> Vec<u32> {
            keys.iter()
                .map(|&usage| {
                    PhysicalKey::from_usage(usage)
                        .and_then(low_level)
                        .unwrap_or(u32::MAX)
                })
                .collect()
        };
        let digits = [
            0x35, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x2D, 0x2E,
        ];
        let top = [
            0x14, 0x1A, 0x08, 0x15, 0x17, 0x1C, 0x18, 0x0C, 0x12, 0x13, 0x2F, 0x30,
        ];
        let home = [
            0x04, 0x16, 0x07, 0x09, 0x0A, 0x0B, 0x0D, 0x0E, 0x0F, 0x33, 0x34, 0x32,
        ];
        let bottom = [
            0x64, 0x1D, 0x1B, 0x06, 0x19, 0x05, 0x11, 0x10, 0x36, 0x37, 0x38,
        ];
        for (keys, rows) in [digits.as_slice(), &top, &home, &bottom]
            .into_iter()
            .zip(super::super::layouts::ROWS)
        {
            assert_eq!(row(keys), rows);
        }
    }

    #[test]
    fn a_usage_no_keyboard_key_stands_in_has_no_place() {
        for usage in [0x66, 0x86, 0xA4, 0xB0, 0xDD] {
            assert_eq!(PhysicalKey::from_usage(usage).and_then(low_level), None);
        }
    }

    #[test]
    fn the_keys_the_handler_treats_specially_are_in_their_places() {
        let place = |usage| PhysicalKey::from_usage(usage).and_then(low_level);
        assert_eq!(place(0x3A), Some(super::super::key::F1));
        assert_eq!(place(0x48), Some(super::super::key::BREAK));
        assert_eq!(place(0x2B), Some(super::super::key::TAB));
        assert_eq!(place(0x53), Some(super::super::key::NUM_LOCK));
        assert_eq!(place(0x47), Some(super::super::key::SCROLL_LOCK));
        assert_eq!(place(0x39), Some(super::super::key::CAPS_LOCK));
        assert_eq!(place(0x2C), Some(super::super::key::SPACE));
        assert_eq!(place(0xE1), Some(super::super::key::LEFT_SHIFT));
        assert_eq!(place(0xE5), Some(super::super::key::RIGHT_SHIFT));
        assert_eq!(place(0xE0), Some(super::super::key::LEFT_CTRL));
        assert_eq!(place(0xE4), Some(super::super::key::RIGHT_CTRL));
        assert_eq!(place(0xE2), Some(super::super::key::LEFT_ALT));
        assert_eq!(place(0xE6), Some(super::super::key::RIGHT_ALT));
        assert_eq!(place(0x88), Some(super::super::key::KANA));
    }
}
