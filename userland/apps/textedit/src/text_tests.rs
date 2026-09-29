//! Unit tests for the grid model of a line.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::ControlFlow;

use super::{
    column_of, for_each_unit, next_row, next_stop, offset_at, prev_row, prev_stop, row_at,
    row_bounds, row_index, row_of, unit_start, word_around, word_end, word_start, Glyph, Row, Unit,
};
use crate::document::{rows_of, Document, MAX_ROW_BYTES};

fn doc(chunks: &[&[u8]]) -> Document {
    Document::from_chunks(chunks.iter().map(|chunk| chunk.to_vec()).collect()).expect("loads")
}

fn units(document: &Document, line: usize, tab: usize) -> Vec<Unit> {
    let mut out = Vec::new();
    for_each_unit(document, document.line_bounds(line), tab, |unit| {
        out.push(*unit);
        ControlFlow::Continue(())
    });
    out
}

/// The line drawn as the grid shows it: characters, tabs as spaces, tokens.
fn drawn(document: &Document, line: usize, tab: usize) -> String {
    let mut out = String::new();
    for unit in units(document, line, tab) {
        match unit.glyph {
            Glyph::Char(ch) => out.push(ch),
            Glyph::Tab => (0..unit.width).for_each(|_| out.push(' ')),
            token => out.push_str(token.token(&mut [0; 12]).expect("a token")),
        }
    }
    out
}

#[test]
fn control_invalid_and_hidden_characters_are_drawn_as_tokens() {
    let document = doc(&[b"a\x03b\xc3(\xe2\x80\xaeZ\x7f\xc2\x85"]);
    assert_eq!(
        drawn(&document, 0, 8),
        "a[x03]b[xC3]([U+202E]Z[x7F][U+0085]"
    );
}

#[test]
fn tabs_advance_to_their_stops_and_wide_characters_take_two_cells() {
    let document = doc(&["a\tb\u{4e2d}c\t".as_bytes()]);
    let columns: Vec<(usize, usize)> = units(&document, 0, 4)
        .iter()
        .map(|u| (u.column, u.width))
        .collect();
    assert_eq!(columns, [(0, 1), (1, 3), (4, 1), (5, 2), (7, 1), (8, 4)]);
}

#[test]
fn a_character_split_across_pieces_is_one_unit() {
    let document = doc(&[b"x\xe4\xb8", b"\xad\xe6", b"\x96\x87y"]);
    assert_eq!(drawn(&document, 0, 8), "x\u{4e2d}\u{6587}y");
    assert_eq!(units(&document, 0, 8).len(), 4);
}

#[test]
fn a_sequence_the_line_ends_inside_is_its_invalid_bytes() {
    let document = doc(&[b"ok\xe4\xb8\nnext"]);
    assert_eq!(drawn(&document, 0, 8), "ok[xE4][xB8]");
    assert_eq!(drawn(&document, 1, 8), "next");
    let broken = doc(&[b"\xe4Q"]);
    assert_eq!(
        drawn(&broken, 0, 8),
        "[xE4]Q",
        "a lead with no continuation is one invalid byte"
    );
}

#[test]
fn a_crlf_is_hidden_as_a_terminator_but_a_lone_cr_is_shown() {
    let document = doc(&[b"one\r\ntwo\rthree"]);
    assert_eq!(drawn(&document, 0, 8), "one");
    assert_eq!(drawn(&document, 1, 8), "two[x0D]three");
}

#[test]
fn columns_and_offsets_invert_each_other() {
    let document = doc(&["\ta\u{4e2d}\x01z".as_bytes()]);
    let bounds = document.line_bounds(0);
    for unit in units(&document, 0, 8) {
        assert_eq!(column_of(&document, bounds, unit.offset, 8), unit.column);
        assert_eq!(
            offset_at(&document, bounds, unit.column * 2, 8),
            unit.offset
        );
    }
    // A unit is split at its middle, however many cells it takes.
    let wide = units(&document, 0, 8)[2];
    assert_eq!(
        offset_at(&document, bounds, wide.column * 2 + 1, 8),
        wide.offset,
        "the wide one's left half"
    );
    assert_eq!(
        offset_at(&document, bounds, wide.column * 2 + 2, 8),
        wide.offset + wide.len,
        "its right half"
    );
    let narrow = units(&document, 0, 8)[1];
    assert_eq!(
        offset_at(&document, bounds, narrow.column * 2 + 1, 8),
        narrow.offset + narrow.len
    );
    let token = units(&document, 0, 8)[3];
    assert_eq!(
        offset_at(&document, bounds, token.column * 2 + 4, 8),
        token.offset
    );
    assert_eq!(
        offset_at(&document, bounds, token.column * 2 + 5, 8),
        token.offset + 1
    );
    assert_eq!(offset_at(&document, bounds, 999, 8), bounds.end);
}

#[test]
fn the_caret_steps_over_whole_units_and_whole_terminators() {
    let document = doc(&[b"a\xe4\xb8\xad\xff\r\nb\n"]);
    let mut at = 0;
    let mut stops = alloc::vec![0];
    while next_stop(&document, at) != at {
        at = next_stop(&document, at);
        stops.push(at);
    }
    assert_eq!(stops, [0, 1, 4, 5, 7, 8, 9]);
    for pair in stops.windows(2) {
        assert_eq!(
            prev_stop(&document, pair[1]),
            pair[0],
            "back from {}",
            pair[1]
        );
    }
    assert_eq!(prev_stop(&document, 0), 0);
}

#[test]
fn words_are_found_forward_backward_and_around() {
    let document = doc(&[b"let snake_case = x1;"]);
    assert_eq!(word_end(&document, 0), 3);
    assert_eq!(word_end(&document, 3), 14);
    assert_eq!(word_start(&document, 14), 4);
    assert_eq!(word_start(&document, 4), 0);
    assert_eq!(word_around(&document, 8), (4, 14));
    assert_eq!(
        word_around(&document, 15),
        (15, 16),
        "a non-word unit selects itself"
    );
    assert_eq!(
        word_around(&document, 20),
        (20, 20),
        "nothing to select at a line's end"
    );
}

#[test]
fn word_motions_cross_one_line_break_at_a_time() {
    let document = doc(&[b"end.\r\n  next"]);
    assert_eq!(word_end(&document, 3), 4);
    assert_eq!(
        word_end(&document, 4),
        6,
        "a line's end steps over its CRLF"
    );
    assert_eq!(word_end(&document, 6), 12);
    assert_eq!(
        word_start(&document, 8),
        6,
        "back past the gap to the line's start"
    );
    assert_eq!(
        word_start(&document, 6),
        4,
        "a line's start steps back over its CRLF"
    );
    assert_eq!(word_start(&document, 4), 0);
}

#[test]
fn a_long_line_is_laid_out_in_rows_that_split_no_character() {
    // A three-byte character straddles the first row boundary.
    let mut line = alloc::vec![b'a'; MAX_ROW_BYTES - 1];
    line.extend_from_slice("\u{4e2d}".as_bytes());
    line.extend(core::iter::repeat_n(b'b', MAX_ROW_BYTES + 3));
    line.extend_from_slice(b"\nshort");
    let document = doc(&[&line]);
    let bounds = document.line_bounds(0);
    assert_eq!(rows_of(bounds.end - bounds.start), 3);
    assert_eq!(document.row_count(), 4);
    let first = row_bounds(&document, Row { line: 0, part: 0 });
    assert_eq!(
        (first.start, first.end, first.next),
        (0, MAX_ROW_BYTES - 1, MAX_ROW_BYTES - 1),
        "the straddling character starts the next row"
    );
    let last = row_bounds(&document, Row { line: 0, part: 2 });
    assert_eq!((last.end, last.next), (bounds.end, bounds.next));
    let mut total = 0;
    for part in 0..3 {
        let row = row_bounds(&document, Row { line: 0, part });
        total += row.end - row.start;
        assert!(row.end - row.start <= MAX_ROW_BYTES + 3);
        assert!(
            drawn_range(&document, row).chars().all(|ch| ch != '['),
            "part {part} split a character"
        );
    }
    assert_eq!(total, bounds.end - bounds.start);

    assert_eq!(
        row_of(&document, first.end),
        Row { line: 0, part: 1 },
        "a boundary belongs to the later row"
    );
    assert_eq!(row_of(&document, first.end - 1), Row { line: 0, part: 0 });
    assert_eq!(
        row_of(&document, bounds.end),
        Row { line: 0, part: 2 },
        "a line's end is on its last row"
    );
    assert_eq!(
        next_row(&document, Row { line: 0, part: 2 }),
        Some(Row { line: 1, part: 0 })
    );
    assert_eq!(
        prev_row(&document, Row { line: 1, part: 0 }),
        Some(Row { line: 0, part: 2 })
    );
    assert_eq!(next_row(&document, Row { line: 1, part: 0 }), None);
    assert_eq!(prev_row(&document, Row::default()), None);

    // Word motions stop at a row boundary and cross it one at a time.
    let second = row_bounds(&document, Row { line: 0, part: 1 });
    assert_eq!(word_end(&document, second.start), second.end);
    assert_eq!(
        word_start(&document, second.start),
        first.start,
        "back across the boundary into the row before"
    );
    assert_eq!(
        word_around(&document, second.start + 5),
        (second.start, second.end)
    );
}

#[test]
fn a_line_exactly_one_row_long_takes_one_row() {
    let document = doc(&[&alloc::vec![b'x'; MAX_ROW_BYTES]]);
    assert_eq!(document.row_count(), 1);
    assert_eq!(row_of(&document, MAX_ROW_BYTES), Row::default());
}

#[test]
fn a_row_s_index_down_the_grid_and_the_row_at_an_index_agree() {
    let mut text = b"one\n".to_vec();
    text.extend(core::iter::repeat_n(b'x', 2 * MAX_ROW_BYTES + 1));
    text.extend_from_slice(b"\ntwo\n");
    let document = doc(&[&text]);
    let rows: Vec<Row> =
        core::iter::successors(Some(Row::default()), |&row| next_row(&document, row)).collect();
    assert_eq!(rows.len(), document.row_count());
    for (index, &row) in rows.iter().enumerate() {
        assert_eq!(row_index(&document, row), index, "{row:?}");
        assert_eq!(row_at(&document, index), row, "row {index}");
    }
    assert_eq!(
        row_at(&document, usize::MAX),
        *rows.last().expect("rows"),
        "past the end is the last row"
    );
}

/// A row drawn as the grid shows it.
fn drawn_range(document: &Document, row: crate::document::LineBounds) -> String {
    let mut out = String::new();
    for_each_unit(document, row, 8, |unit| {
        match unit.glyph {
            Glyph::Char(ch) => out.push(ch),
            glyph => out.push_str(glyph.token(&mut [0; 12]).unwrap_or(" ")),
        }
        ControlFlow::Continue(())
    });
    out
}

#[test]
fn an_offset_inside_a_character_or_a_crlf_snaps_to_its_start() {
    let document = doc(&[b"a\xf0\x9f\x98\x80b\r\nc\xe4"]);
    let starts: Vec<usize> = (0..=document.len())
        .map(|at| unit_start(&document, at))
        .collect();
    assert_eq!(starts, [0, 1, 1, 1, 1, 5, 6, 6, 8, 9, 10]);
}
