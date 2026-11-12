//! Screens copied as they draw, and text typed as the keyboard can.

use super::{copied, typing, Typing};
use crate::front::MAX_TYPED;
use crate::link::typeable;
use alloc::string::ToString;
use alloc::vec::Vec;

/// A screen `columns` wide of `rows`, each padded with spaces.
fn screen(columns: usize, rows: &[&[u8]]) -> Vec<u8> {
    rows.iter()
        .flat_map(|row| {
            let mut cells = row.to_vec();
            cells.resize(columns, b' ');
            cells
        })
        .collect()
}

fn typed(text: &str) -> Vec<u8> {
    typing(text.as_bytes()).unwrap().unwrap().codes
}

fn typing_of(text: &str) -> Typing {
    typing(text.as_bytes()).unwrap().unwrap()
}

#[test]
fn a_screen_is_copied_a_line_a_row_without_the_blanks_round_its_text() {
    let cells = screen(8, &[b"C:\\>dir", b"", b"  x", b"", b""]);
    assert_eq!(copied(8, &cells).unwrap(), "C:\\>dir\n\n  x");
}

#[test]
fn every_cell_is_copied_as_it_draws() {
    let cells = [0x01, 0x03, 0x00, 0x82, 0xDB, 0xFF, b'a', 0xFF];
    assert_eq!(
        copied(8, &cells).unwrap(),
        "\u{263A}\u{2665} \u{E9}\u{2588}\u{A0}a",
        "a blank no-break space ends no row"
    );
}

#[test]
fn a_blank_screen_copies_as_nothing() {
    assert_eq!(copied(4, &[b' ', 0, 0xFF, b' ', 0, 0, 0, 0]).unwrap(), "");
}

#[test]
fn text_is_typed_as_its_own_codes() {
    assert_eq!(typed("DIR /W"), b"DIR /W");
    assert_eq!(
        typed("caf\u{E9} \u{BD}"),
        [b'c', b'a', b'f', 0x82, b' ', 0xAB]
    );
}

#[test]
fn every_line_break_presses_enter_once_and_a_tab_tab() {
    assert_eq!(typed("a\r\nb\nc\rd\u{2028}e"), b"a\rb\rc\rd\re");
    assert_eq!(typed("a\tb"), b"a\tb");
    assert_eq!(typed("\r\n\r\n"), b"\r\r");
}

#[test]
fn typographic_punctuation_is_typed_plainly_spelt() {
    assert_eq!(
        typed("\u{201C}don\u{2019}t\u{201D} \u{2014} wait\u{2026}"),
        b"\"don't\" - wait..."
    );
    assert_eq!(typed("\u{2022} item"), [0xF9, b' ', b'i', b't', b'e', b'm']);
}

#[test]
fn a_letter_the_page_lacks_is_typed_without_its_accents() {
    assert_eq!(typed("\u{101}\u{151}\u{10D}"), b"aoc");
}

#[test]
fn a_combining_accent_changes_the_letter_before_it_where_the_page_has_both_as_one() {
    let typing = typing_of("e\u{301}a\u{304}\u{301}x\u{308}\u{301}");
    assert_eq!(typing.codes, [0x82, b'a', b'x']);
    assert_eq!(
        typing.left_out, 0,
        "an accent the page lacks is dropped, not counted"
    );
    assert_eq!(typed("u\u{308}"), [0x81]);
}

#[test]
fn what_no_key_types_as_itself_is_left_out_and_counted() {
    let typing = typing_of("\u{263A} \u{2190}\u{1B}[0m\u{1F600}\u{141}");
    assert_eq!(typing.codes, b" [0m");
    assert_eq!(
        typing.left_out, 5,
        "the face, the arrow, Escape, the emoji, the stroked L"
    );
    assert!(!typing.cut);
}

#[test]
fn what_draws_nothing_passes_unseen() {
    let typing = typing_of("\u{FEFF}a\u{200B}b\u{AD}");
    assert_eq!(
        typing,
        Typing {
            codes: b"ab".to_vec(),
            left_out: 0,
            cut: false
        }
    );
}

#[test]
fn text_past_the_most_typed_is_cut_where_a_whole_character_ends() {
    let typing = typing_of(&"a".repeat(MAX_TYPED + 5));
    assert_eq!(typing.codes.len(), MAX_TYPED);
    assert!(typing.cut);
    let exact = typing_of(&"a".repeat(MAX_TYPED));
    assert_eq!((exact.codes.len(), exact.cut), (MAX_TYPED, false));
    let mut ending = "a".repeat(MAX_TYPED - 1);
    ending.push('\u{2026}');
    let typing = typing_of(&ending);
    assert_eq!(typing.codes.len(), MAX_TYPED - 1, "no part of the ellipsis");
    assert!(typing.cut);
}

#[test]
fn no_more_is_read_than_the_most_typed_takes_whatever_follows() {
    let emoji = "\u{1F600}".repeat(MAX_TYPED * 3);
    let read = typing_of(&emoji);
    assert_eq!(read.left_out, MAX_TYPED, "what was read, and no further");
    assert!(read.cut);
    let mut long = "a".repeat(MAX_TYPED * 4 - 1);
    long.push('\u{E9}');
    let cut_char = typing_of(&long);
    assert!(cut_char.cut && cut_char.codes.iter().all(|&code| code == b'a'));
    assert_eq!(typing(b"ok\xFF\xFEno").unwrap(), None, "not UTF-8");
}

#[test]
fn what_was_not_typed_is_told_in_a_sentence() {
    let told = |left_out, cut| {
        Typing {
            codes: Vec::new(),
            left_out,
            cut,
        }
        .shortfall()
    };
    assert_eq!(told(0, false), None);
    let (_, one) = told(1, false).unwrap();
    assert!(one.starts_with("1 character could"), "{one}");
    let (headline, both) = told(3, true).unwrap();
    assert_eq!(headline, "Only the start of the text is being typed");
    assert!(
        both.contains("3 characters") && both.ends_with("machine."),
        "{both}"
    );
}

#[test]
fn every_character_a_screen_draws_and_a_key_types_comes_back_as_its_code() {
    for code in (0..=u8::MAX).filter(|&code| typeable(code) && !matches!(code, b'\t' | b'\r')) {
        let glyph = tairix_cp437::glyph_of(code);
        assert_eq!(typed(&glyph.to_string()), [code], "{code:#04x}");
    }
}
