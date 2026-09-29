//! Unit tests for a window's colouring, with the real lexer standing in for
//! the sandbox that runs it.

use alloc::vec::Vec;

use tairix_sandbox::textsyntax::{LexedBatch, MAX_LEX_BATCH_LINES};
use tairix_syntax::{lex_line, Format, LineState, Span, MAX_LEX_LINE};
use tairix_theme::SyntaxRole;

use super::{stretch, Highlight, LexJob, LineEdit, Stopped, FAR_LINES};
use crate::document::Document;

fn doc(text: &[u8]) -> Document {
    Document::from_chunks(alloc::vec![text.to_vec()]).expect("loads")
}

/// Replace `range` with `text`, reporting the edit.
fn edit(
    document: &mut Document,
    highlight: &mut Highlight,
    range: core::ops::Range<usize>,
    text: &[u8],
) -> crate::document::Change {
    let removal = LineEdit::removing(document, range.clone());
    let change = document.replace(range, text).expect("room");
    highlight.edited(removal.inserted(document, text.len()));
    change
}

/// What the sandbox would answer.
fn answer(job: &LexJob) -> LexedBatch {
    let mut batch = LexedBatch::default();
    let mut state = job.state;
    for line in job.lines() {
        state = lex_line(job.format, state, line, &mut batch.spans);
        batch.lines.push((batch.spans.len(), state));
    }
    batch
}

/// Adopting a batch answers the lines it coloured, and a stale one none.
#[test]
fn adopting_answers_the_lines_it_coloured() {
    let document =
        Document::from_chunks(alloc::vec![b"a = 1\nb = 2\nc = 3\n".to_vec()]).expect("loads");
    let mut highlight = Highlight::new(Format::Toml);
    let job = highlight
        .next_job(&document, 0..3)
        .expect("room")
        .expect("a batch");
    assert_eq!(highlight.adopt(job.id + 1, &answer(&job)), 0..0);
    let lines = highlight.adopt(job.id, &answer(&job));
    assert_eq!(lines.start, 0);
    assert!(lines.end >= 3, "the lines shown were coloured: {lines:?}");
}

/// Answer every batch until the view is coloured; how many it took.
fn settle(highlight: &mut Highlight, document: &Document, view: core::ops::Range<usize>) -> usize {
    let mut jobs = 0;
    while let Some(job) = highlight.next_job(document, view.clone()).expect("room") {
        highlight.adopt(job.id, &answer(&job));
        jobs += 1;
        assert!(jobs < 10_000, "colouring never settles");
    }
    jobs
}

/// Each line's spans lexed in one pass from the top: what the view must show.
fn expected(format: Format, text: &[u8]) -> Vec<Vec<Span>> {
    let mut state = LineState::START;
    text.split(|&b| b == b'\n')
        .map(|line| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let mut spans = Vec::new();
            state = lex_line(format, state, line, &mut spans);
            spans
        })
        .collect()
}

fn roles(spans: Option<&[Span]>) -> Vec<SyntaxRole> {
    spans.unwrap_or(&[]).iter().map(|span| span.role).collect()
}

fn program(lines: usize) -> Vec<u8> {
    let mut text = Vec::new();
    for at in 0..lines {
        match at % 5 {
            0 => text.extend_from_slice(b"/* a comment\n"),
            1 => text.extend_from_slice(b"   still open */ fn f() {}\n"),
            2 => text.extend_from_slice(b"let s = \"text\"; // tail\n"),
            3 => text.extend_from_slice(b"struct Point { x: u32 }\n"),
            _ => text.extend_from_slice(b"\n"),
        }
    }
    text
}

#[test]
fn the_view_is_coloured_as_one_pass_from_the_top_would() {
    let text = program(400);
    let document = doc(&text);
    let mut highlight = Highlight::new(Format::Rust);
    settle(&mut highlight, &document, 300..340);
    let want = expected(Format::Rust, &text);
    for (line, want) in want.iter().enumerate().take(340).skip(300) {
        assert_eq!(highlight.spans(line), Some(&want[..]), "line {line}");
    }
}

/// Lines longer than a lexer reads, so a batch holds only a few dozen.
fn long_lines(count: usize) -> Vec<u8> {
    let mut text = Vec::new();
    for _ in 0..count {
        text.resize(text.len() + MAX_LEX_LINE + 4096, b'x');
        text.push(b'\n');
    }
    text
}

#[test]
fn a_refill_of_long_lines_carries_on_until_it_reaches_the_view() {
    let document = doc(&long_lines(200));
    let mut highlight = Highlight::new(Format::Rust);
    settle(&mut highlight, &document, 150..160);
    settle(&mut highlight, &document, 40..50);
    for line in 40..50 {
        assert!(
            highlight.spans(line).is_some(),
            "checkpoint refill: line {line}"
        );
    }

    let mut text = alloc::vec![b'\n'; FAR_LINES + 40];
    text.extend_from_slice(&long_lines(120));
    let document = doc(&text);
    let mut highlight = Highlight::new(Format::Rust);
    let view = FAR_LINES + 100..FAR_LINES + 110;
    settle(&mut highlight, &document, view.clone());
    for line in view {
        assert!(
            highlight.spans(line).is_some(),
            "provisional refill: line {line}"
        );
    }
}

#[test]
fn an_edit_above_where_a_refill_stopped_restarts_it() {
    let mut document = doc(&long_lines(200));
    let mut highlight = Highlight::new(Format::Rust);
    settle(&mut highlight, &document, 150..160);
    settle(&mut highlight, &document, 40..50);
    let at = document.line_start(10);
    edit(&mut document, &mut highlight, at..at, b"/*");
    settle(&mut highlight, &document, 40..50);
    let want = expected(Format::Rust, &document.to_vec().expect("room"));
    for (line, want) in want.iter().enumerate().take(50).skip(40) {
        assert_eq!(highlight.spans(line), Some(&want[..]), "line {line}");
    }
}

#[test]
fn plain_text_asks_for_nothing() {
    let document = doc(b"anything\nat all");
    let mut highlight = Highlight::new(Format::PlainText);
    assert!(highlight.next_job(&document, 0..2).expect("room").is_none());
}

#[test]
fn a_batch_never_passes_its_bounds() {
    let text = alloc::vec![b'\n'; MAX_LEX_BATCH_LINES * 3];
    let document = doc(&text);
    let mut highlight = Highlight::new(Format::Rust);
    let job = highlight
        .next_job(&document, 0..MAX_LEX_BATCH_LINES * 3)
        .expect("room")
        .expect("a job");
    assert_eq!(job.len(), MAX_LEX_BATCH_LINES);
    assert!(
        highlight
            .next_job(&document, 0..10)
            .expect("room")
            .is_none(),
        "one batch in flight at a time"
    );
}

#[test]
fn a_line_longer_than_a_lexer_reads_is_cut_and_the_next_found_by_index() {
    let mut text = b"let a = 1;\n".to_vec();
    text.extend(core::iter::repeat_n(b'x', MAX_LEX_LINE * 3));
    text.extend_from_slice(b"\r\nfn g() {}\r\nlast\r");
    let document = doc(&text);
    let mut highlight = Highlight::new(Format::Rust);
    let job = highlight
        .next_job(&document, 0..4)
        .expect("room")
        .expect("a job");
    let lines: Vec<&[u8]> = job.lines().collect();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0], b"let a = 1;");
    assert_eq!(lines[1].len(), MAX_LEX_LINE);
    assert_eq!(lines[2], b"fn g() {}", "a CRLF's CR is not sent");
    assert_eq!(lines[3], b"last\r", "a lone CR at the end is content");
    highlight.adopt(job.id, &answer(&job));
    let want = expected(Format::Rust, &text);
    assert_eq!(highlight.spans(2), Some(&want[2][..]));
}

#[test]
fn a_far_view_is_coloured_provisionally_then_exactly() {
    // A comment opened at the top stays open for the whole document, so only
    // the exact pass colours the far view as comment.
    let mut text = b"/* opened here\n".to_vec();
    for _ in 0..FAR_LINES * 3 {
        text.extend_from_slice(b"fn inside() {}\n");
    }
    let document = doc(&text);
    let mut highlight = Highlight::new(Format::Rust);
    let far = FAR_LINES * 2..FAR_LINES * 2 + 10;
    let first = highlight
        .next_job(&document, far.clone())
        .expect("room")
        .expect("a job");
    assert_eq!(
        first.state,
        LineState::START,
        "a far view does not wait for the frontier"
    );
    highlight.adopt(first.id, &answer(&first));
    assert!(
        roles(highlight.spans(far.start)).contains(&SyntaxRole::Keyword),
        "provisionally, code"
    );
    settle(&mut highlight, &document, far.clone());
    assert_eq!(
        roles(highlight.spans(far.start)),
        [SyntaxRole::Comment],
        "exactly, still inside the comment"
    );
}

#[test]
fn an_answer_holds_only_for_the_lines_before_an_edit_made_while_it_was_asked() {
    let text = program(100);
    let mut document = doc(&text);
    let mut highlight = Highlight::new(Format::Rust);
    let job = highlight
        .next_job(&document, 0..100)
        .expect("room")
        .expect("a job");
    let at = document.line_start(40);
    edit(&mut document, &mut highlight, at..at, b"\"");
    highlight.adopt(job.id, &answer(&job));
    assert!(highlight.spans(39).is_some());
    let job = highlight
        .next_job(&document, 0..100)
        .expect("room")
        .expect("the edit is lexed again");
    let edited = document.line_bounds(40);
    let mut line = Vec::new();
    document
        .copy_range(edited.start..edited.end, &mut line)
        .expect("room");
    assert_eq!(
        job.lines().next(),
        Some(&line[..]),
        "resumed at the edited line"
    );
    highlight.adopt(job.id, &answer(&job));
    settle(&mut highlight, &document, 0..100);
    let want = expected(Format::Rust, &document.to_vec().expect("room"));
    for (line, want) in want.iter().enumerate().take(100) {
        assert_eq!(highlight.spans(line), Some(&want[..]), "line {line}");
    }
}

#[test]
fn an_edit_moves_kept_spans_with_their_lines() {
    let text = b"let a = \"one\";\nlet b = 2;\nlet c = 3;\n".to_vec();
    let mut document = doc(&text);
    let mut highlight = Highlight::new(Format::Rust);
    settle(&mut highlight, &document, 0..4);
    let string = *highlight
        .spans(0)
        .expect("coloured")
        .iter()
        .find(|span| span.role == SyntaxRole::String)
        .expect("a string");

    // Typing inside the string stretches it; nothing is lexed yet.
    let at = string.start as usize + 2;
    edit(&mut document, &mut highlight, at..at, b"XY");
    let stretched = highlight
        .spans(0)
        .expect("still coloured")
        .iter()
        .find(|span| span.role == SyntaxRole::String)
        .copied();
    assert_eq!(
        stretched.map(|span| (span.start, span.end)),
        Some((string.start, string.end + 2))
    );

    // A new line above moves the lines below it down with their colours.
    let top = highlight.spans(0).expect("coloured").to_vec();
    let below = highlight.spans(2).expect("coloured").to_vec();
    let change = edit(&mut document, &mut highlight, 0..0, b"\n\n");
    assert_eq!(
        highlight.spans(2),
        Some(&top[..]),
        "the line pushed down keeps its colours"
    );
    assert_eq!(highlight.spans(4), Some(&below[..]));

    // Undo takes them back up.
    let removal = LineEdit::removing(&document, change.at..change.at + change.inserted_len());
    document.revert(&change);
    highlight.edited(removal.inserted(&document, change.removed_len()));
    assert_eq!(highlight.spans(0), Some(&top[..]));
    assert_eq!(highlight.spans(2), Some(&below[..]));

    // Breaking a line before its string carries the string's colours to
    // the new line; joining it again gives the line its colours back.
    let string = top
        .iter()
        .find(|span| span.role == SyntaxRole::String)
        .copied()
        .expect("a string");
    let at = string.start as usize;
    edit(&mut document, &mut highlight, at..at, b"\n    ");
    let moved = highlight.spans(1).and_then(|spans| spans.first()).copied();
    assert_eq!(
        moved.map(|span| (span.start, span.role)),
        Some((4, SyntaxRole::String))
    );
    let end = document.line_bounds(0).end;
    edit(&mut document, &mut highlight, end..end + 5, b"");
    assert_eq!(highlight.spans(0), Some(&top[..]));
}

#[test]
fn spans_stretch_over_typing_inside_them_and_are_clipped_by_a_removal() {
    let span = |start, end| Span {
        start,
        end,
        role: SyntaxRole::String,
    };
    let mut spans = alloc::vec![span(2, 6), span(8, 10)];
    stretch(&mut spans, 4, 0, 3);
    assert_eq!(spans, [span(2, 9), span(11, 13)]);
    let mut spans = alloc::vec![span(2, 6), span(8, 10)];
    stretch(&mut spans, 6, 0, 1);
    assert_eq!(
        spans,
        [span(2, 6), span(9, 11)],
        "typing at a span's end does not join it"
    );
    let mut spans = alloc::vec![span(2, 6), span(8, 10)];
    stretch(&mut spans, 4, 5, 0);
    assert_eq!(
        spans,
        [span(2, 4), span(4, 5)],
        "a removal clips what it cuts"
    );
}

#[test]
fn scrolling_back_refills_from_a_checkpoint() {
    let text = program(2000);
    let document = doc(&text);
    let mut highlight = Highlight::new(Format::Rust);
    settle(&mut highlight, &document, 1900..1950);
    assert!(
        highlight.spans(10).is_none(),
        "far above the view nothing is kept"
    );
    let job = highlight
        .next_job(&document, 500..550)
        .expect("room")
        .expect("a refill");
    assert_eq!(
        job.len(),
        550 - 448,
        "from the checkpoint at or before the view"
    );
    highlight.adopt(job.id, &answer(&job));
    settle(&mut highlight, &document, 500..550);
    let want = expected(Format::Rust, &text);
    for (line, want) in want.iter().enumerate().take(550).skip(500) {
        assert_eq!(highlight.spans(line), Some(&want[..]), "line {line}");
    }
}

#[test]
fn repeated_failure_stops_colouring_and_says_why() {
    let document = doc(b"fn main() {}\n");
    let mut highlight = Highlight::new(Format::Rust);
    for _ in 0..3 {
        let job = highlight
            .next_job(&document, 0..1)
            .expect("room")
            .expect("a job");
        highlight.failed(job.id);
    }
    assert_eq!(highlight.stopped(), Some(Stopped::Failed));
    assert!(highlight.next_job(&document, 0..1).expect("room").is_none());
    highlight.set_format(Format::Rust);
    assert_eq!(
        highlight.stopped(),
        None,
        "choosing a format again starts over"
    );
}

#[test]
fn an_answer_to_a_superseded_batch_is_dropped() {
    let document = doc(b"fn main() {}\n");
    let mut highlight = Highlight::new(Format::Rust);
    let job = highlight
        .next_job(&document, 0..1)
        .expect("room")
        .expect("a job");
    highlight.set_format(Format::Python);
    highlight.adopt(job.id, &answer(&job));
    assert!(highlight.spans(0).is_none());
    let next = highlight
        .next_job(&document, 0..1)
        .expect("room")
        .expect("a fresh job");
    assert_ne!(next.id, job.id);
}
