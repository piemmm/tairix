//! The editing: a free cursor, typing in and over, lines split and joined,
//! a selection made, indented and cleared, and the window kept over the
//! cursor.
#![allow(
    clippy::redundant_closure_for_method_calls,
    reason = "a method's path is not general over the editor's lifetimes"
)]

use super::super::fixture::{empty, store, whole};
use super::*;

/// `bytes` in a store of room to spare, edited by `edit`; the document
/// and the editor's state after.
fn edited(
    bytes: &[u8],
    editor: Editor,
    edit: impl FnOnce(&mut Edit<'_, '_, super::super::fixture::Bytes>),
) -> (Vec<u8>, Editor) {
    let mut store = store(2048);
    let mut text = empty(&mut store);
    text.insert(0, bytes).unwrap();
    let mut editor = Editor {
        lines: 1 + super::super::text::line_ends(bytes),
        ..editor
    };
    edit(&mut Edit {
        editor: &mut editor,
        text: &mut text,
    });
    (whole(&mut text), editor)
}

fn at(line: u32, start: u32, column: u16) -> Editor {
    Editor {
        cursor: Spot {
            start,
            line,
            column,
        },
        goal: column,
        ..Editor::default()
    }
}

fn spot(line: u32, start: u32, column: u16) -> Spot {
    Spot {
        start,
        line,
        column,
    }
}

#[test]
fn typing_past_a_lines_end_fills_it_with_blanks() {
    let (text, editor) = edited(b"ab\r\ncd", at(0, 0, 5), |edit| edit.type_in(b"x").unwrap());
    assert_eq!(text, b"ab   x\r\ncd");
    assert_eq!((editor.cursor.column, editor.modified), (6, true));
    let over = Editor {
        insert: false,
        ..at(0, 0, 1)
    };
    let (text, _) = edited(b"abc", over, |edit| edit.type_in(b"XY").unwrap());
    assert_eq!(
        text, b"aXY",
        "over the text there, the line grown at its end"
    );
    let (text, editor) = edited(b"a\r\nb", at(1, 3, 1), |edit| {
        edit.type_in(b"1\r\n2\r\n").unwrap();
    });
    assert_eq!(text, b"a\r\nb1\r\n2\r\n");
    assert_eq!(
        (editor.cursor, editor.lines),
        (spot(3, 10, 0), 4),
        "the lines pasted counted from the cursor's"
    );
}

#[test]
fn enter_splits_the_line_and_keeps_its_indent() {
    let (text, editor) = edited(b"    if x then y", at(0, 0, 13), |edit| {
        edit.enter().unwrap();
    });
    assert_eq!(text, b"    if x then\r\n     y");
    assert_eq!(editor.cursor, spot(1, 15, 4));
    let (text, editor) = edited(b"ab", at(0, 0, 1), |edit| edit.split().unwrap());
    assert_eq!(
        (text, editor.cursor),
        (b"a\r\nb".to_vec(), spot(0, 0, 1)),
        "Ctrl+N leaves the cursor"
    );
}

#[test]
fn backspace_and_del_join_lines_at_their_ends() {
    let (text, editor) = edited(b"zero\r\none\r\ntwo", at(2, 11, 0), |edit| edit.back());
    assert_eq!(text, b"zero\r\nonetwo");
    assert_eq!(
        editor.cursor,
        spot(1, 6, 3),
        "on the line before, found from the cursor's"
    );
    let (text, _) = edited(b"one\r\ntwo", at(0, 0, 6), |edit| edit.delete().unwrap());
    assert_eq!(
        text, b"one   two",
        "the line filled out to the cursor first"
    );
    let (text, editor) = edited(b"ab", at(0, 0, 7), |edit| edit.back());
    assert_eq!(
        (text, editor.cursor.column),
        (b"ab".to_vec(), 6),
        "past the end, only the cursor moves"
    );
    let (text, editor) = edited(b"one\ntwo", at(0, 0, 3), |edit| edit.delete().unwrap());
    assert_eq!(
        (text, editor.lines),
        (b"onetwo".to_vec(), 1),
        "a lone LF joined"
    );
}

#[test]
fn shift_selects_and_typing_replaces_the_selection() {
    let (text, editor) = edited(b"hello world", at(0, 0, 0), |edit| {
        edit.go(Move::WordRight, true);
        assert_eq!(edit.selection(), Some(0..6));
        assert_eq!(edit.copy(false), Some(b"hello ".to_vec()));
        edit.type_in(b"big ").unwrap();
    });
    assert_eq!(text, b"big world");
    assert_eq!(editor.anchor, None);
    let (text, editor) = edited(b"one\r\ntwo\r\nthree", at(0, 0, 1), |edit| {
        edit.go(Move::Down, true);
        edit.go(Move::Down, true);
        assert_eq!(edit.copy(true), Some(b"ne\r\ntwo\r\nt".to_vec()));
    });
    assert_eq!(text, b"ohree", "cut across lines");
    assert_eq!((editor.cursor, editor.lines), (spot(0, 0, 1), 1));
    let backwards = Editor {
        anchor: Some(spot(2, 10, 2)),
        ..at(0, 0, 1)
    };
    let (text, editor) = edited(b"one\r\ntwo\r\nthree", backwards, |edit| {
        assert!(edit.clear());
    });
    assert_eq!(
        (text, editor.cursor),
        (b"oree".to_vec(), spot(0, 0, 1)),
        "the cursor at the earlier end"
    );
}

#[test]
fn moves_keep_their_column_and_the_window_follows() {
    let lines: Vec<u8> = (0..30)
        .flat_map(|n| alloc::format!("line {n}\r\n").into_bytes())
        .collect();
    let editor = Editor {
        rows: 10,
        ..at(0, 0, 3)
    };
    let (_, editor) = edited(&lines, editor, |edit| {
        edit.go(Move::PageDown, false);
        edit.go(Move::Down, false);
    });
    assert_eq!((editor.cursor.line, editor.cursor.column), (11, 3));
    assert_eq!(
        editor.top.line, 10,
        "scrolled a page, then the cursor still in view"
    );
    let (_, editor) = edited(
        &lines,
        Editor {
            rows: 10,
            ..at(0, 0, 0)
        },
        |edit| edit.go(Move::Bottom, false),
    );
    assert_eq!(
        (editor.cursor.line, editor.top.line),
        (30, 21),
        "the last line shown at the window's foot"
    );
    let (_, editor) = edited(b"  indented", at(0, 0, 9), |edit| {
        edit.go(Move::Home, false);
        assert_eq!(edit.editor.cursor.column, 2, "to the text");
        edit.go(Move::Home, false);
    });
    assert_eq!(editor.cursor.column, 0, "then to the margin");
    let (_, editor) = edited(b"a\tb", at(0, 0, 0), |edit| edit.go(Move::End, false));
    assert_eq!(editor.cursor.column, 9, "a tab reaches the next stop");
}

#[test]
fn the_window_moves_by_its_own_measure() {
    let lines: Vec<u8> = (0..30)
        .flat_map(|n| alloc::format!("{n:02}\r\n").into_bytes())
        .collect();
    let editor = Editor {
        rows: 10,
        columns: 20,
        ..at(5, 20, 2)
    };
    let (_, editor) = edited(&lines, editor, |edit| {
        edit.go(Move::WindowBottom, false);
        assert_eq!(edit.editor.cursor.line, 9, "the window's foot");
        edit.go(Move::WindowTop, false);
        assert_eq!(edit.editor.cursor.line, 0);
        edit.go(Move::NextLine, false);
        assert_eq!(edit.editor.cursor, spot(1, 4, 0));
        edit.go(Move::PageRight, false);
        assert_eq!((edit.editor.left, edit.editor.cursor.column), (20, 20));
        edit.go(Move::PageLeft, false);
        assert_eq!((edit.editor.left, edit.editor.cursor.column), (0, 0));
        edit.scroll_line(true);
        edit.scroll_line(true);
    });
    assert_eq!(
        (editor.top.line, editor.cursor.line),
        (2, 2),
        "scrolled off, the cursor carried into the window"
    );
    let (_, editor) = edited(
        &lines,
        Editor {
            rows: 10,
            ..at(0, 0, 1)
        },
        |edit| edit.go_to(25, 1),
    );
    assert_eq!((editor.cursor, editor.top.line), (spot(25, 100, 1), 16));
}

#[test]
fn a_long_line_is_reached_to_its_end() {
    let long = alloc::vec![b'x'; 300];
    let (_, editor) = edited(&long, at(0, 0, 299), |edit| {
        edit.go(Move::Right, false);
        edit.go(Move::Right, false);
    });
    assert_eq!(
        editor.cursor.column, 300,
        "past column 256 while the line is there"
    );
    let (_, editor) = edited(b"ab", at(0, 0, MOST_COLUMN), |edit| {
        edit.go(Move::Right, false);
    });
    assert_eq!(editor.cursor.column, MOST_COLUMN, "a short line's reach");
}

#[test]
fn lines_are_cut_whole_or_to_their_end() {
    let (text, _) = edited(b"one\r\ntwo\r\nthree", at(1, 5, 2), |edit| {
        assert_eq!(edit.cut_line(), b"two\r\n");
    });
    assert_eq!(text, b"one\r\nthree");
    let (text, editor) = edited(b"one\r\ntwo", at(0, 0, 1), |edit| {
        assert_eq!(edit.cut_to_end(), b"ne");
    });
    assert_eq!((text, editor.lines), (b"o\r\ntwo".to_vec(), 2));
}

#[test]
fn a_selection_is_indented_and_outdented_whole_lines_at_a_time() {
    let selected = Editor {
        anchor: Some(spot(0, 0, 0)),
        ..at(2, 12, 0)
    };
    let (text, editor) = edited(b"one\r\n  two\r\nthree", selected, |edit| {
        edit.indent().unwrap();
    });
    assert_eq!(
        text, b"\tone\r\n\t  two\r\nthree",
        "the line the selection ends at the start of left alone"
    );
    assert_eq!(
        (editor.anchor, editor.cursor),
        (Some(spot(0, 0, 0)), spot(2, 14, 0)),
        "the selection kept over the lines"
    );
    let (text, editor) = edited(&text, editor, |edit| edit.outdent());
    assert_eq!(text, b"one\r\n  two\r\nthree");
    assert_eq!(editor.cursor, spot(2, 12, 0));
    let inside = Editor {
        anchor: Some(spot(0, 0, 1)),
        ..at(1, 4, 4)
    };
    let (text, editor) = edited(b"ab\r\n    cd", inside, |edit| {
        edit.indent().unwrap();
        assert_eq!(edit.selection(), Some(2..10), "still from the b to the c");
    });
    assert_eq!(text, b"\tab\r\n\t    cd");
    assert_eq!(editor.anchor, Some(spot(0, 0, 9)));
    let (text, editor) = edited(b"        x", at(0, 0, 9), |edit| edit.outdent());
    assert_eq!(
        (text, editor.cursor.column),
        (b"x".to_vec(), 1),
        "the cursor's own line, a stop's blanks"
    );
}

#[test]
fn a_spot_is_found_from_one_before_or_after_it() {
    edited(b"a\r\nb\r\nc\r\nd", at(2, 6, 0), |edit| {
        assert_eq!(edit.spot_at(10), spot(3, 9, 1), "after the cursor");
        assert_eq!(edit.spot_at(1), spot(0, 0, 1), "before it");
        assert_eq!(edit.spot_near(4, spot(1, 3, 0)), spot(1, 3, 1));
    });
}

#[test]
fn a_cr_typed_alone_is_a_character_and_the_cursor_stays_after_it() {
    let (text, editor) = edited(b"ab", at(0, 0, 2), |edit| {
        edit.type_in(b"\r").unwrap();
        edit.type_in(b"c").unwrap();
    });
    assert_eq!(text, b"ab\rc", "no blank filled in before the next");
    assert_eq!(editor.cursor, spot(0, 0, 4));
    let (text, editor) = edited(b"ab", at(0, 0, 2), |edit| {
        edit.type_in(b"\r").unwrap();
        edit.type_in(b"\n").unwrap();
    });
    assert_eq!(
        (text, editor.cursor, editor.lines),
        (b"ab\r\n".to_vec(), spot(1, 4, 0), 2),
        "a CR then an LF make a line's end"
    );
}
