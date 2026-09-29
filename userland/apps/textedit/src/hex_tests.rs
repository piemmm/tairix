//! Unit tests for the hex view model.

use super::{
    digit_value, hex_pair, rows, step_left, step_right, type_byte, type_digit, HexCaret, HexLayout,
    Nibble, Pane, BYTES_PER_ROW,
};

const fn caret(offset: usize, pane: Pane, nibble: Nibble) -> HexCaret {
    HexCaret {
        offset,
        pane,
        nibble,
    }
}

#[test]
fn a_whole_number_of_rows_leaves_a_row_for_the_end_caret() {
    assert_eq!(rows(0), 1);
    assert_eq!(rows(15), 1);
    assert_eq!(rows(16), 2);
    assert_eq!(rows(33), 3);
}

#[test]
fn the_offset_column_widens_past_eight_digits_only_when_it_must() {
    assert_eq!(HexLayout::for_len(0).offset_digits(), 8);
    assert_eq!(HexLayout::for_len(0xffff_ffff).offset_digits(), 8);
    assert_eq!(HexLayout::for_len(0x1_0000_0000).offset_digits(), 9);
    let layout = HexLayout::for_len(0x10);
    assert_eq!(layout.offset_text(0x1f0, &mut [0; 16]), "000001F0");
}

#[test]
fn a_row_lays_out_as_offset_two_groups_and_ascii() {
    let layout = HexLayout::for_len(100);
    assert_eq!(layout.hex_column(0), 10);
    assert_eq!(layout.hex_column(7), 31);
    assert_eq!(
        layout.hex_column(8),
        35,
        "a gap between the two groups of eight"
    );
    assert_eq!(layout.hex_column(15), 56);
    assert_eq!(layout.ascii_column(0), 61);
    assert_eq!(layout.width(), 61 + BYTES_PER_ROW + 1);
}

#[test]
fn a_click_lands_on_the_nibble_or_the_byte_under_it() {
    let layout = HexLayout::for_len(100);
    assert_eq!(
        layout.hit(0, 3, 100),
        None,
        "the offset column holds no stop"
    );
    assert_eq!(
        layout.hit(1, 10, 100),
        Some(caret(16, Pane::Hex, Nibble::High))
    );
    assert_eq!(
        layout.hit(1, 11, 100),
        Some(caret(16, Pane::Hex, Nibble::Low))
    );
    assert_eq!(
        layout.hit(1, 12, 100),
        Some(caret(16, Pane::Hex, Nibble::Low)),
        "the gap after a byte is its low half"
    );
    assert_eq!(
        layout.hit(1, 35, 100),
        Some(caret(24, Pane::Hex, Nibble::High))
    );
    assert_eq!(
        layout.hit(0, 62, 100),
        Some(caret(1, Pane::Ascii, Nibble::High))
    );
    assert_eq!(
        layout.hit(6, 20, 100),
        Some(caret(99, Pane::Hex, Nibble::Low)),
        "the last byte"
    );
    assert_eq!(
        layout.hit(6, 41, 100),
        Some(caret(100, Pane::Hex, Nibble::High)),
        "clamped to the end"
    );
    for stop in [
        caret(5, Pane::Hex, Nibble::Low),
        caret(9, Pane::Hex, Nibble::High),
        caret(13, Pane::Ascii, Nibble::High),
    ] {
        assert_eq!(layout.hit(0, layout.caret_column(stop), 100), Some(stop));
    }
}

#[test]
fn digits_overwrite_a_nibble_at_a_time_and_insert_a_byte_at_a_time() {
    let high = caret(4, Pane::Hex, Nibble::High);
    let over = type_digit(high, Some(0x12), 0xa, false);
    assert_eq!(
        (over.range, over.byte, over.caret),
        (4..5, 0xa2, caret(4, Pane::Hex, Nibble::Low))
    );
    let low = type_digit(over.caret, Some(0xa2), 0xb, false);
    assert_eq!(
        (low.range, low.byte, low.caret),
        (4..5, 0xab, caret(5, Pane::Hex, Nibble::High))
    );

    let insert = type_digit(high, Some(0x12), 0xc, true);
    assert_eq!((insert.range, insert.byte), (4..4, 0xc0));
    let fill = type_digit(insert.caret, Some(0xc0), 0xd, true);
    assert_eq!((fill.range, fill.byte, fill.caret.offset), (4..5, 0xcd, 5));

    let append = type_digit(caret(9, Pane::Hex, Nibble::High), None, 0x7, false);
    assert_eq!(
        (append.range, append.byte),
        (9..9, 0x70),
        "past the end a digit appends"
    );
}

#[test]
fn ascii_typing_overwrites_or_inserts_a_byte() {
    let at = caret(2, Pane::Ascii, Nibble::High);
    assert_eq!(type_byte(at, Some(b'x'), b'y', false).range, 2..3);
    assert_eq!(type_byte(at, Some(b'x'), b'y', true).range, 2..2);
    assert_eq!(type_byte(at, None, b'y', false).range, 2..2);
    assert_eq!(
        type_byte(at, Some(b'x'), b'y', false).caret,
        caret(3, Pane::Ascii, Nibble::High)
    );
}

#[test]
fn the_caret_steps_by_nibble_in_hex_and_by_byte_in_ascii() {
    let start = caret(0, Pane::Hex, Nibble::High);
    let mut at = start;
    let mut seen = alloc::vec![at];
    for _ in 0..5 {
        at = step_right(at, 2);
        seen.push(at);
    }
    assert_eq!(
        seen,
        [
            start,
            caret(0, Pane::Hex, Nibble::Low),
            caret(1, Pane::Hex, Nibble::High),
            caret(1, Pane::Hex, Nibble::Low),
            caret(2, Pane::Hex, Nibble::High),
            caret(2, Pane::Hex, Nibble::High),
        ]
    );
    assert_eq!(
        step_left(caret(2, Pane::Hex, Nibble::High)),
        caret(1, Pane::Hex, Nibble::Low)
    );
    assert_eq!(step_left(start), start);
    assert_eq!(
        step_right(caret(0, Pane::Ascii, Nibble::High), 2),
        caret(1, Pane::Ascii, Nibble::High)
    );
    assert_eq!(
        step_left(caret(1, Pane::Ascii, Nibble::High)),
        caret(0, Pane::Ascii, Nibble::High)
    );
}

#[test]
fn bytes_show_as_digits_and_typed_digits_read_back() {
    assert_eq!(hex_pair(0x0f), *b"0F");
    assert_eq!(digit_value('e'), Some(14));
    assert_eq!(digit_value('E'), Some(14));
    assert_eq!(digit_value('G'), None);
    assert_eq!(
        digit_value('\u{ff10}'),
        None,
        "a full-width digit is not a hex digit"
    );
}
