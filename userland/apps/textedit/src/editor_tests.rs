//! Unit tests for the editor's commands, against the text they leave.

use alloc::vec::Vec;

use tairix_syntax::{Diagnostic, Format, Severity};

use super::{Command, Conversion, Converted, Editor, Lines, Mode, Motion, Refusal};
use crate::detect::{Indent, LineEnding};
use crate::document::{Document, OutOfMemory, Snapshot};
use crate::hex::{Nibble, Pane};
use crate::selection::Selection;

fn editor(text: &[u8]) -> Editor {
    Editor::new(
        Document::from_chunks(alloc::vec![text.to_vec()]).expect("loads"),
        Format::PlainText,
    )
}

/// Run a conversion of `snapshot` to `to` through, `budget` bytes a step.
fn convert_by(
    snapshot: &Snapshot,
    to: LineEnding,
    budget: usize,
) -> Result<Option<Vec<Vec<u8>>>, OutOfMemory> {
    let mut conversion = Conversion::new(to);
    for _ in 0..=2 * snapshot.len() + 2 {
        if let Converted::Done(result) = conversion.step(snapshot, budget) {
            return result;
        }
    }
    panic!("a conversion of {} bytes never finished", snapshot.len());
}

fn convert(snapshot: &Snapshot, to: LineEnding) -> Result<Option<Vec<Vec<u8>>>, OutOfMemory> {
    convert_by(snapshot, to, usize::MAX)
}

fn text(editor: &Editor) -> Vec<u8> {
    editor.document().to_vec().expect("room")
}

fn run(editor: &mut Editor, command: &Command) -> super::Effect {
    editor.run(command, 10)
}

fn go(editor: &mut Editor, motion: Motion) {
    run(
        editor,
        &Command::Move {
            motion,
            extend: false,
        },
    );
}

fn type_str(editor: &mut Editor, typed: &str) {
    for ch in typed.chars() {
        run(editor, &Command::Type(ch));
    }
}

fn at(editor: &mut Editor, offset: usize) {
    editor.click(offset, false);
}

#[test]
fn a_typed_word_undoes_as_one_step_and_reports_one_line() {
    let mut editor = editor(b"one\ntwo\n");
    at(&mut editor, 4);
    let effect = run(&mut editor, &Command::Type('X'));
    assert_eq!(
        effect.text,
        Some(Lines {
            first: 1,
            last: Some(1)
        })
    );
    type_str(&mut editor, "YZ");
    assert_eq!(text(&editor), b"one\nXYZtwo\n");
    assert!(editor.is_modified());
    run(&mut editor, &Command::Undo);
    assert_eq!(text(&editor), b"one\ntwo\n");
    assert_eq!(editor.selection(), Selection::caret(4));
    assert!(!editor.is_modified());
    run(&mut editor, &Command::Redo);
    assert_eq!(text(&editor), b"one\nXYZtwo\n");
}

#[test]
fn a_new_line_keeps_the_indentation_and_the_convention() {
    let mut editor = editor(b"fn f() {\r\n    body();\r\n}\r\n");
    assert_eq!(editor.line_ending(), LineEnding::CrLf);
    let end = editor.document().line_bounds(1).end;
    at(&mut editor, end);
    let effect = run(&mut editor, &Command::Newline);
    assert_eq!(
        effect.text.map(|lines| lines.last),
        Some(None),
        "lines below moved"
    );
    type_str(&mut editor, "more();");
    assert_eq!(
        text(&editor),
        b"fn f() {\r\n    body();\r\n    more();\r\n}\r\n"
    );
}

#[test]
fn tab_indents_to_the_next_stop_or_the_selected_lines() {
    let mut editor = editor(b"ab\ncd\nef\n");
    run(&mut editor, &Command::SetIndent(Indent::Spaces(4)));
    at(&mut editor, 1);
    run(&mut editor, &Command::Tab);
    assert_eq!(
        text(&editor),
        b"a   b\ncd\nef\n",
        "to the next multiple of four"
    );
    editor.click(0, false);
    editor.click(8, true);
    run(&mut editor, &Command::Tab);
    assert_eq!(
        text(&editor),
        b"    a   b\n    cd\nef\n",
        "a selection ending at a line's start leaves that line"
    );
    run(&mut editor, &Command::Backtab);
    assert_eq!(text(&editor), b"a   b\ncd\nef\n");
    run(&mut editor, &Command::SetIndent(Indent::Tab));
    run(&mut editor, &Command::Indent);
    assert_eq!(text(&editor), b"\ta   b\n\tcd\nef\n");
}

#[test]
fn comments_toggle_at_the_shallowest_indentation() {
    let mut editor = editor(b"fn f() {\n    a();\n\n        b();\n}\n");
    run(&mut editor, &Command::SetFormat(Format::Rust));
    let start = editor.document().line_start(1);
    let end = editor.document().line_start(4);
    editor.click(start, false);
    editor.click(end, true);
    run(&mut editor, &Command::ToggleComment);
    assert_eq!(
        text(&editor),
        b"fn f() {\n    // a();\n\n    //     b();\n}\n"
    );
    run(&mut editor, &Command::ToggleComment);
    assert_eq!(text(&editor), b"fn f() {\n    a();\n\n        b();\n}\n");
    run(&mut editor, &Command::SetFormat(Format::Json));
    assert_eq!(
        run(&mut editor, &Command::ToggleComment).refused,
        Some(Refusal::NoLineComment)
    );
}

#[test]
fn commenting_the_caret_line_keeps_the_caret_on_its_text() {
    let mut editor = editor(b"  value = 1\n");
    run(&mut editor, &Command::SetFormat(Format::Toml));
    at(&mut editor, 4);
    run(&mut editor, &Command::ToggleComment);
    assert_eq!(text(&editor), b"  # value = 1\n");
    assert_eq!(editor.selection(), Selection::caret(6));
}

#[test]
fn deleting_takes_whole_characters_and_whole_line_breaks() {
    let mut editor = editor("a\u{4e2d}\r\nb".as_bytes());
    at(&mut editor, 6);
    run(&mut editor, &Command::Backspace);
    assert_eq!(
        text(&editor),
        "a\u{4e2d}b".as_bytes(),
        "the CRLF goes as one"
    );
    run(&mut editor, &Command::Backspace);
    assert_eq!(text(&editor), b"ab");
}

#[test]
fn word_deletion_takes_the_word_before_or_after_the_caret() {
    let mut words = editor(b"let value = 1");
    at(&mut words, 9);
    run(&mut words, &Command::DeleteWordLeft);
    assert_eq!(text(&words), b"let  = 1");
    run(&mut words, &Command::DeleteWordRight);
    assert_eq!(
        text(&words),
        b"let ",
        "past the gap, punctuation included, and the word after it"
    );
}

#[test]
fn vertical_motion_keeps_its_column_across_a_short_line() {
    let mut editor = editor(b"abcdef\nab\nabcdef\n");
    at(&mut editor, 5);
    go(&mut editor, Motion::Down);
    assert_eq!(
        editor.selection().head,
        9,
        "clamped to the short line's end"
    );
    go(&mut editor, Motion::Down);
    assert_eq!(editor.selection().head, 15, "back to the goal column");
    go(&mut editor, Motion::PageDown);
    assert_eq!(editor.selection().head, 17, "the last row, at its end");
    go(&mut editor, Motion::Down);
    assert_eq!(editor.selection().head, 17);
    go(&mut editor, Motion::PageUp);
    assert_eq!(
        editor.selection().head,
        5,
        "the top row, at the goal column"
    );
    go(&mut editor, Motion::PageUp);
    assert_eq!(editor.selection().head, 0, "then the document's start");
}

#[test]
fn home_goes_to_the_indentation_then_to_the_line_start() {
    let mut editor = editor(b"    text\n");
    at(&mut editor, 6);
    go(&mut editor, Motion::LineStart);
    assert_eq!(editor.selection().head, 4);
    go(&mut editor, Motion::LineStart);
    assert_eq!(editor.selection().head, 0);
    go(&mut editor, Motion::LineEnd);
    assert_eq!(editor.selection().head, 8);
}

#[test]
fn a_binary_document_opens_in_hex_and_digits_overwrite_nibbles() {
    let mut editor = editor(b"\x00\x11\x22\x33");
    assert_eq!(editor.mode(), Mode::Hex);
    assert!(editor.overwrite());
    type_str(&mut editor, "ab");
    type_str(&mut editor, "C");
    assert_eq!(text(&editor), b"\xab\xc1\x22\x33");
    assert_eq!(editor.hex_caret().nibble, Nibble::Low);
    run(&mut editor, &Command::Undo);
    assert_eq!(text(&editor), b"\x00\x11\x22\x33", "one run, one step");
    run(&mut editor, &Command::ToggleOverwrite);
    assert!(!editor.overwrite());
    at(&mut editor, 0);
    assert_eq!(
        editor.hex_caret().nibble,
        Nibble::High,
        "a click starts on a byte's first digit"
    );
    type_str(&mut editor, "ff");
    assert_eq!(
        text(&editor),
        b"\xff\x00\x11\x22\x33",
        "inserting puts a new byte in"
    );
    type_str(&mut editor, "zq");
    assert_eq!(
        text(&editor),
        b"\xff\x00\x11\x22\x33",
        "what is not a digit is not typed"
    );
}

#[test]
fn the_ascii_pane_types_bytes_and_text_mode_snaps_the_caret() {
    let mut editor = editor("\0x\u{4e2d}".as_bytes());
    editor.set_pane(Pane::Ascii);
    at(&mut editor, 1);
    run(&mut editor, &Command::Type('Q'));
    assert_eq!(text(&editor), "\0Q\u{4e2d}".as_bytes());
    at(&mut editor, 3);
    run(&mut editor, &Command::SetMode(Mode::Text));
    assert_eq!(
        editor.selection(),
        Selection::caret(2),
        "inside a character, snapped to its start"
    );
}

#[test]
fn replace_all_is_one_step() {
    let mut editor = editor(b"cat hat cat\ncat\n");
    let effect = editor.replace_all(&[0..3, 8..11, 12..15], b"dog");
    assert_eq!(text(&editor), b"dog hat dog\ndog\n");
    assert_eq!(
        effect.text,
        Some(Lines {
            first: 0,
            last: Some(1)
        })
    );
    run(&mut editor, &Command::Undo);
    assert_eq!(text(&editor), b"cat hat cat\ncat\n");
    run(&mut editor, &Command::Redo);
    assert_eq!(text(&editor), b"dog hat dog\ndog\n");
}

#[test]
fn cut_copy_and_paste_move_bytes_through_the_selection() {
    let mut editor = editor(b"hello world");
    editor.click(0, false);
    editor.click(5, true);
    assert_eq!(editor.copy(), Ok(b"hello".to_vec()));
    let (_, cut) = editor.cut();
    assert_eq!(cut.as_deref(), Some(&b"hello"[..]));
    assert_eq!(text(&editor), b" world");
    at(&mut editor, 6);
    editor.replace_selection(b"!!");
    assert_eq!(text(&editor), b" world!!");
    run(&mut editor, &Command::SelectAll);
    assert_eq!(editor.copy().map(|bytes| bytes.len()), Ok(8));
}

#[test]
fn a_save_marks_the_document_saved_only_as_it_was_snapshotted() {
    let mut editor = editor(b"text");
    type_str(&mut editor, "a");
    let (generation, _) = editor.snapshot().expect("room");
    editor.saved(generation);
    assert!(!editor.is_modified());
    type_str(&mut editor, "b");
    let (older, _) = editor.snapshot().expect("room");
    type_str(&mut editor, "c");
    editor.saved(older);
    assert!(
        editor.is_modified(),
        "the file holds an older state than the window"
    );
}

#[test]
fn line_endings_convert_across_pieces_and_keep_a_lone_cr() {
    let document = Document::from_chunks(alloc::vec![b"a\r".to_vec(), b"\nb\nc\rd\r".to_vec()])
        .expect("loads");
    let mut source = Editor::new(document, Format::PlainText);
    let (generation, snapshot) = source.snapshot().expect("room");
    let flat = |chunks: Option<Vec<Vec<u8>>>| chunks.expect("breaks to change").concat();
    assert_eq!(
        flat(convert(&snapshot, LineEnding::Lf).expect("room")),
        b"a\nb\nc\rd\r"
    );
    let crlf = convert(&snapshot, LineEnding::CrLf).expect("room");
    assert_eq!(flat(crlf.clone()), b"a\r\nb\r\nc\rd\r");
    let was = source.line_ending();
    source.converted(generation, crlf, LineEnding::CrLf);
    assert_eq!(text(&source), b"a\r\nb\r\nc\rd\r");
    assert_eq!(source.line_ending(), LineEnding::CrLf);
    run(&mut source, &Command::Undo);
    assert_eq!(text(&source), b"a\r\nb\nc\rd\r");
    assert_eq!(
        source.line_ending(),
        was,
        "undo restores the convention too"
    );
    run(&mut source, &Command::Redo);
    assert_eq!(
        source.line_ending(),
        LineEnding::CrLf,
        "and redo reapplies it"
    );
    assert_eq!(
        source.converted(generation, None, LineEnding::Lf).refused,
        Some(Refusal::Changed)
    );
}

#[test]
fn a_document_already_in_a_convention_is_neither_copied_nor_changed() {
    for (text, to) in [
        (&b"a\nb\n"[..], LineEnding::Lf),
        (b"a\r\nb\r\nlone\rcr", LineEnding::CrLf),
        (b"no breaks at all", LineEnding::CrLf),
    ] {
        let mut source = editor(text);
        let (generation, snapshot) = source.snapshot().expect("room");
        let converted = convert(&snapshot, to).expect("room");
        assert!(converted.is_none(), "{text:?} is already {to:?}");
        source.converted(generation, converted, to);
        assert_eq!(
            source.line_ending(),
            to,
            "the convention new breaks take is set"
        );
        assert!(!source.is_modified(), "{text:?} stays unmodified");
    }
    let mixed =
        Document::from_chunks(alloc::vec![b"a\r".to_vec(), b"\nb\nc".to_vec()]).expect("loads");
    let mut source = Editor::new(mixed, Format::PlainText);
    let (_, snapshot) = source.snapshot().expect("room");
    assert_eq!(
        convert(&snapshot, LineEnding::CrLf)
            .expect("room")
            .map(|chunks| chunks.concat()),
        Some(b"a\r\nb\r\nc".to_vec()),
        "a CRLF split across pieces is one break, and the lone LF is normalised"
    );
}

#[test]
fn diagnostics_are_kept_only_for_the_text_they_describe() {
    let mut editor = editor(b"a = 1\nb\nc = 3\n");
    run(&mut editor, &Command::SetFormat(Format::AppSettings));
    assert!(editor.wants_check());
    let generation = editor.generation();
    let problem = Diagnostic {
        line: Some(2),
        severity: Severity::Warning,
        message: "this line is ignored".into(),
    };
    editor.checked(generation, alloc::vec![problem.clone()]);
    assert!(!editor.wants_check());
    assert_eq!(editor.diagnostics(), (&[problem][..], true));
    run(&mut editor, &Command::NextProblem);
    assert_eq!(editor.selection(), Selection::caret(6));
    run(&mut editor, &Command::NextProblem);
    assert_eq!(
        editor.selection(),
        Selection::caret(6),
        "wrapping to the only problem"
    );
    type_str(&mut editor, "x");
    assert!(editor.wants_check());
    editor.checked(generation, Vec::new());
    assert_eq!(
        editor.diagnostics().0.len(),
        1,
        "an answer about older text is dropped"
    );
}

#[test]
fn going_to_a_line_clamps_to_the_document() {
    let mut editor = editor(b"one\ntwo\nthree");
    run(&mut editor, &Command::GoToLine(2));
    assert_eq!(editor.selection(), Selection::caret(4));
    run(&mut editor, &Command::GoToLine(99));
    assert_eq!(editor.selection(), Selection::caret(8));
    run(&mut editor, &Command::GoToLine(0));
    assert_eq!(editor.selection(), Selection::caret(0));
}

#[test]
fn an_edit_past_the_bound_is_refused_and_changes_nothing() {
    let mut editor = editor(b"x");
    let huge = alloc::vec![b'y'; super::MAX_EDIT_BYTES + 1];
    assert_eq!(
        editor.replace_selection(&huge).refused,
        Some(Refusal::TooLarge)
    );
    assert_eq!(text(&editor), b"x");
}

#[test]
fn relief_lets_go_of_the_text_only_trimmed_undo_steps_named() {
    let mut editor = editor(&[b'a'; 8192]);
    for round in 0..super::PRESSURE_UNDO_STEPS + 3 {
        let fill = b'b' + u8::try_from(round % 20).expect("small");
        let effect = editor.converted(
            editor.generation,
            Some(alloc::vec![alloc::vec![fill; 8192]]),
            LineEnding::Lf,
        );
        assert_eq!(effect.refused, None);
    }
    let held = editor.document().held();
    editor.adopt_pressure(tairix_reclaim::PressureBand::Mild);
    assert_eq!(
        editor.document().held(),
        held - 3 * 8192,
        "the three trimmed steps alone named the load and the first two conversions"
    );
    assert!(run(&mut editor, &Command::Undo).refused.is_none());
}

#[test]
fn pressure_that_eases_leaves_the_history_whole() {
    use tairix_reclaim::PressureBand;
    let mut editor = editor(b"");
    editor.adopt_pressure(PressureBand::Severe);
    for _ in 0..super::PRESSURE_UNDO_STEPS + 3 {
        editor.replace_selection(b"x");
        editor.history.close_run();
    }
    assert!(run(&mut editor, &Command::Undo).refused.is_none());
    let depth = editor.history.depth();
    for band in [PressureBand::Moderate, PressureBand::Normal] {
        editor.adopt_pressure(band);
        assert_eq!(editor.history.depth(), depth, "{band:?}");
        assert!(editor.history.can_redo(), "{band:?}");
    }
    editor.adopt_pressure(PressureBand::Mild);
    assert_eq!(editor.history.depth(), super::PRESSURE_UNDO_STEPS);
    assert!(!editor.history.can_redo(), "pressure arriving drops redo");
}

#[test]
fn up_down_and_end_keep_the_caret_on_the_rows_of_a_long_line() {
    use crate::document::MAX_ROW_BYTES;
    use crate::text::{row_of, Row};
    let mut text = b"ab\n".to_vec();
    text.resize(3 + 2 * MAX_ROW_BYTES, b'x');
    let mut editor = editor(&text);
    let row = |editor: &Editor| row_of(editor.document(), editor.selection().head);
    go(&mut editor, Motion::DocumentEnd);
    assert_eq!(row(&editor), Row { line: 1, part: 1 });
    go(&mut editor, Motion::Up);
    assert_eq!(
        row(&editor),
        Row { line: 1, part: 0 },
        "up reaches the row above"
    );
    go(&mut editor, Motion::Up);
    assert_eq!(
        row(&editor),
        Row { line: 0, part: 0 },
        "and on up, never stuck"
    );
    go(&mut editor, Motion::Down);
    assert_eq!(row(&editor), Row { line: 1, part: 0 });
    go(&mut editor, Motion::Down);
    assert_eq!(row(&editor), Row { line: 1, part: 1 });
    go(&mut editor, Motion::Up);
    go(&mut editor, Motion::LineEnd);
    assert_eq!(
        row(&editor),
        Row { line: 1, part: 0 },
        "end stays on its row"
    );
    go(&mut editor, Motion::LineEnd);
    assert_eq!(
        row(&editor),
        Row { line: 1, part: 0 },
        "a second end does not walk down"
    );
}

#[test]
fn an_undo_never_leaves_the_text_caret_inside_a_character() {
    use crate::hex::HexCaret;
    let mut editor = editor("\u{e9}".as_bytes());
    run(&mut editor, &Command::SetMode(Mode::Hex));
    editor.click_hex(
        HexCaret {
            offset: 1,
            pane: Pane::Hex,
            nibble: Nibble::High,
        },
        false,
    );
    run(&mut editor, &Command::Type('4'));
    assert_eq!(text(&editor), [0xc3, 0x49]);
    run(&mut editor, &Command::SetMode(Mode::Text));
    run(&mut editor, &Command::Undo);
    assert_eq!(text(&editor), "\u{e9}".as_bytes());
    let head = editor.selection().head;
    assert!(
        head == 0 || head == 2,
        "the caret stands on a stop, not at {head}"
    );
}

#[test]
fn a_click_past_the_end_lands_at_the_end() {
    let mut editor = editor(b"ab");
    editor.click(4, false);
    assert_eq!(editor.selection(), Selection::caret(2));
    editor.click(usize::MAX, true);
    assert_eq!(editor.selection(), Selection::caret(2));
}

#[test]
fn choosing_the_setting_already_in_force_changes_nothing() {
    let mut editor = editor(b"a\tb");
    let format = editor.format();
    assert_eq!(
        run(&mut editor, &Command::SetFormat(format)),
        super::Effect::default()
    );
    let tab = editor.tab_width();
    let width = u8::try_from(tab).expect("a small width");
    assert_eq!(
        run(&mut editor, &Command::SetTabWidth(width)),
        super::Effect::default()
    );
    assert!(
        run(&mut editor, &Command::SetTabWidth(width + 1)).view,
        "a new width reshows"
    );
}

#[test]
fn an_empty_paste_is_no_change_at_all() {
    let mut editor = editor(b"text");
    assert_eq!(editor.replace_selection(b""), super::Effect::default());
    assert!(!editor.is_modified());
    assert_eq!(
        run(&mut editor, &Command::Undo),
        super::Effect::default(),
        "no step to undo"
    );
}

#[test]
fn a_conversion_stepped_a_byte_at_a_time_matches_one_taken_whole() {
    let texts: [&[u8]; 5] = [
        b"a\r\nb\nc\rd\r",
        b"\r\n\r\n\n\r",
        b"lone\rcr only",
        b"ends in\r",
        b"x\ny\r\nz",
    ];
    for text in texts {
        for split in 0..=text.len() {
            let (head, tail) = text.split_at(split);
            let mut document =
                Document::from_chunks(alloc::vec![head.to_vec(), tail.to_vec()]).expect("loads");
            let snapshot = document.snapshot().expect("room");
            for to in [LineEnding::Lf, LineEnding::CrLf] {
                let whole = convert(&snapshot, to).expect("room");
                for budget in 1..=4 {
                    assert_eq!(
                        convert_by(&snapshot, to, budget).expect("room"),
                        whole,
                        "{text:?} split at {split}, {budget} a step, to {to:?}"
                    );
                }
            }
        }
    }
}
