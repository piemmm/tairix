//! The settings file: read as untrusted text, written back whole.

use super::*;

#[test]
fn the_settings_are_read_back_as_they_were_written() {
    let settings = Settings {
        colour: 0x1E,
        bars: false,
        tab: 4,
    };
    assert_eq!(parse(&compose(settings), Settings::default()), settings);
    assert_eq!(
        compose(settings),
        b"[EDIT]\r\nColors=1E\r\nScrollBars=0\r\nTabStops=4\r\n"
    );
}

#[test]
fn a_line_it_cannot_take_keeps_the_setting_as_it_was() {
    let base = Settings::default();
    let text = b"colors = 4f\nScrollBars=7\nTabStops=0\nTABSTOPS=12\njunk\n=\nColors=FFF\n";
    assert_eq!(
        parse(text, base),
        Settings {
            colour: 0x4F,
            bars: base.bars,
            tab: 12
        }
    );
    assert_eq!(
        parse(b"Colors=80", base).colour,
        base.colour,
        "no blinking text"
    );
    let mut long = alloc::vec![b' '; LEN];
    long.extend_from_slice(b"\nTabStops=3");
    assert_eq!(
        parse(&long, base).tab,
        base.tab,
        "nothing past the first bytes read"
    );
}

#[test]
fn the_file_lies_beside_the_program() {
    assert_eq!(
        beside(b"C:\\DOS\\EDIT.COM", 128),
        Some(b"C:\\DOS\\EDIT.INI".to_vec())
    );
    assert_eq!(beside(b"A:EDIT.COM", 128), Some(b"A:EDIT.INI".to_vec()));
    assert_eq!(
        beside(b"C:\\DOS\\EDIT.COM", 10),
        None,
        "a path too long for the room"
    );
}
