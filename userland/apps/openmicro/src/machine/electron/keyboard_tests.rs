use super::*;

/// The address that selects `lines`, every other line's bit high.
fn selecting(lines: &[u8]) -> u16 {
    lines
        .iter()
        .fold(0xBFFF, |addr, &line| addr & !(1u16 << line))
}

#[test]
fn a_line_selected_by_its_low_address_bit_reads_its_keys_down() {
    let mut keyboard = Keyboard::new();
    keyboard.set(Key::A, true);
    keyboard.set(Key::SHIFT, true);
    assert_eq!(
        keyboard.read(selecting(&[12])),
        0x04,
        "A is line 12's third key"
    );
    assert_eq!(
        keyboard.read(selecting(&[13])),
        0x08,
        "SHIFT is line 13's fourth"
    );
    assert_eq!(
        keyboard.read(selecting(&[12, 13])),
        0x0C,
        "lines selected together OR"
    );
    assert_eq!(keyboard.read(selecting(&[0])), 0, "nothing down on line 0");
    assert_eq!(keyboard.read(0xFFFF), 0, "no line selected");
    keyboard.set(Key::A, false);
    assert_eq!(keyboard.read(selecting(&[12])), 0);
}

#[test]
fn every_key_has_its_own_place_and_the_two_unwired_places_none() {
    assert_eq!(Key::ALL.len(), 4 * LINES - 2);
    for (n, key) in Key::ALL.iter().enumerate() {
        assert!(!Key::ALL[n + 1..].contains(key), "{key:?} twice");
        assert_eq!(Key::from_number(key.number()), Some(*key));
        assert!(key.line() < 14 && !key.label().is_empty());
    }
    assert_eq!(Key::from_number(2), None);
    assert_eq!(Key::from_number(11), None);
}

#[test]
fn text_types_through_the_keys_and_modifiers_the_os_reads() {
    assert_eq!(Key::typing('Q'), Some((Key::Q, Modifier::None)));
    assert_eq!(Key::typing('@'), Some((Key::DIGIT_0, Modifier::Shift)));
    assert_eq!(Key::typing('~'), Some((Key::LEFT, Modifier::Ctrl)));
    assert_eq!(Key::typing('['), Some((Key::COPY, Modifier::Shift)));
    assert_eq!(Key::typing('\r'), Some((Key::RETURN, Modifier::None)));
    assert_eq!(Key::typing('q'), None, "CAPS LOCK types capitals");
    assert_eq!(Key::typing('\u{e9}'), None);
}

#[test]
fn a_restored_keyboard_holds_only_keys_it_has() {
    use crate::snapshot::{self, Reader, SnapshotError, Writer};
    let restore = |keyboard: &Keyboard| -> Result<Keyboard, SnapshotError> {
        let mut w = Writer::new();
        w.put(keyboard);
        let body = snapshot::open(&w.finish()?)?;
        Reader::new(&body).get()
    };
    let mut keyboard = Keyboard::new();
    keyboard.set(Key::Z, true);
    assert_eq!(restore(&keyboard), Ok(keyboard.clone()));
    for (line, keys) in [(0, 0x04), (2, 0x08), (5, 0x10)] {
        let mut stray = keyboard.clone();
        stray.lines[line] |= keys;
        assert_eq!(
            restore(&stray),
            Err(SnapshotError::Malformed),
            "line {line}"
        );
    }
}
