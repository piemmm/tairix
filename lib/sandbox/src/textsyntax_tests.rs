//! Unit tests for the text-syntax service: the real worker behind the
//! loopback seam, and a hostile one for every reply invariant.

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::Cell;

use tairix_syntax::{lex_line, Format, LineState, Severity, Span, MAX_DIAGNOSTICS};
use tairix_theme::SyntaxRole;

use super::{
    detect, lex_lines, validate_document, SyntaxFailure, TextSyntaxService, MAX_HEAD_LEN,
    MAX_LEX_BATCH_BYTES, MAX_LEX_BATCH_LINES, MAX_VALIDATE_LEN,
};
use crate::host::ParserSandbox;
use crate::loopback::LoopbackLauncher;
use crate::testing::{loopback, scripted, NullSink, Scripted};
use crate::wire::Writer;
use crate::worker::Service;

fn sandbox() -> ParserSandbox<LoopbackLauncher<fn() -> TextSyntaxService>, NullSink> {
    loopback()
}

/// A lex reply for one line carrying `spans` as raw `(start, end, role)`.
fn lex_reply(spans: &[(u32, u32, u8)]) -> Vec<u8> {
    let mut w = Writer::new();
    w.u8(super::REPLY_LEXED);
    w.u32(1);
    w.u32(0);
    w.u32(u32::try_from(spans.len()).expect("few spans"));
    for &(start, end, role) in spans {
        w.u32(start);
        w.u32(end);
        w.u8(role);
    }
    w.finish()
}

#[test]
fn a_lexed_batch_is_what_the_lexer_answers_locally() {
    let lines: [&[u8]; 3] = [b"/* open", b"close */ int x = 1;", b"\"str\""];
    let batch = lex_lines(&mut sandbox(), Format::C, LineState::START, &lines).expect("lexes");
    let mut state = LineState::START;
    for (at, line) in lines.iter().enumerate() {
        let mut local = Vec::new();
        state = lex_line(Format::C, state, line, &mut local);
        assert_eq!(batch.line(at), local.as_slice(), "line {at}");
        assert_eq!(batch.lines[at].1, state);
    }
}

#[test]
fn an_empty_batch_is_answered_empty() {
    let batch = lex_lines(&mut sandbox(), Format::Rust, LineState::START, &[]).expect("lexes");
    assert!(batch.spans.is_empty() && batch.lines.is_empty());
    assert!(batch.line(0).is_empty());
}

#[test]
fn a_batch_past_its_bounds_is_never_sent() {
    let wide = alloc::vec![b'x'; MAX_LEX_BATCH_BYTES / 2 + 1];
    let two: [&[u8]; 2] = [&wide, &wide];
    assert_eq!(
        lex_lines(&mut sandbox(), Format::C, LineState::START, &two),
        Err(SyntaxFailure::TooLarge)
    );
    let many = alloc::vec![&b""[..]; MAX_LEX_BATCH_LINES + 1];
    assert_eq!(
        lex_lines(&mut sandbox(), Format::C, LineState::START, &many),
        Err(SyntaxFailure::TooLarge)
    );
    let long = alloc::vec![b'x'; tairix_syntax::MAX_LEX_LINE + 1];
    assert_eq!(
        lex_lines(&mut sandbox(), Format::C, LineState::START, &[&long]),
        Err(SyntaxFailure::TooLarge)
    );
}

#[test]
fn a_span_that_breaks_the_contract_fails_the_whole_reply() {
    let line: [&[u8]; 1] = [b"abcdef"];
    let comment = SyntaxRole::Comment.index();
    for spans in [
        alloc::vec![(0, 7, comment)],                   // past the line
        alloc::vec![(2, 2, comment)],                   // empty
        alloc::vec![(3, 5, comment), (1, 2, comment)],  // descending
        alloc::vec![(0, 4, comment), (3, 5, comment)],  // overlapping
        alloc::vec![(0, 2, SyntaxRole::Plain.index())], // a plain span
        alloc::vec![(0, 2, 200)],                       // no such role
    ] {
        let reply = lex_reply(&spans);
        assert_eq!(
            lex_lines(&mut scripted(reply), Format::C, LineState::START, &line),
            Err(SyntaxFailure::ReplyMalformed),
            "{spans:?}"
        );
    }
    let fine = lex_reply(&[(0, 2, comment), (2, 6, SyntaxRole::Keyword.index())]);
    let batch =
        lex_lines(&mut scripted(fine), Format::C, LineState::START, &line).expect("believed");
    assert_eq!(
        batch.line(0),
        [
            Span {
                start: 0,
                end: 2,
                role: SyntaxRole::Comment
            },
            Span {
                start: 2,
                end: 6,
                role: SyntaxRole::Keyword
            },
        ]
    );
}

#[test]
fn a_reply_that_cannot_be_believed_retires_its_worker() {
    let launched = Rc::new(Cell::new(0u32));
    let counted = Rc::clone(&launched);
    let mut sandbox = ParserSandbox::new(
        LoopbackLauncher::new(move || {
            counted.set(counted.get() + 1);
            Scripted(alloc::vec![super::REPLY_DETECTED, 99])
        }),
        NullSink,
    );
    assert_eq!(
        detect(&mut sandbox, b"x"),
        Err(SyntaxFailure::ReplyMalformed)
    );
    assert!(!sandbox.is_live(), "the liar was retired");
    let _ = detect(&mut sandbox, b"x");
    assert_eq!(launched.get(), 2, "the next request met a fresh worker");
    let refused = alloc::vec![super::REPLY_REFUSED];
    let mut refusing = scripted(refused);
    assert_eq!(detect(&mut refusing, b"x"), Err(SyntaxFailure::Refused));
    assert_eq!(detect(&mut refusing, b"x"), Err(SyntaxFailure::Refused));
}

#[test]
fn a_reply_for_the_wrong_number_of_lines_or_with_trailing_bytes_fails() {
    let line: [&[u8]; 1] = [b"x"];
    let mut two = Writer::new();
    two.u8(super::REPLY_LEXED);
    two.u32(2);
    assert_eq!(
        lex_lines(
            &mut scripted(two.finish()),
            Format::C,
            LineState::START,
            &line
        ),
        Err(SyntaxFailure::ReplyMalformed)
    );
    let mut trailing = lex_reply(&[]);
    trailing.push(0);
    assert_eq!(
        lex_lines(&mut scripted(trailing), Format::C, LineState::START, &line),
        Err(SyntaxFailure::ReplyMalformed)
    );
    assert_eq!(
        lex_lines(
            &mut scripted(alloc::vec![super::REPLY_REFUSED]),
            Format::C,
            LineState::START,
            &line
        ),
        Err(SyntaxFailure::Refused)
    );
}

#[test]
fn validation_is_the_system_verdict_carried_across() {
    let refused = validate_document(&mut sandbox(), Format::SystemConfig, b"os.loginType x\n")
        .expect("answers");
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].line, Some(1));
    assert_eq!(refused[0].severity, Severity::Error);
    let clean = validate_document(&mut sandbox(), Format::SystemConfig, b"os.loginType text\n")
        .expect("answers");
    assert!(clean.is_empty());
    let whole = validate_document(
        &mut sandbox(),
        Format::FontFamily,
        b"label = M\nkind = monospace\n",
    )
    .expect("answers");
    assert_eq!(whole.first().map(|d| d.line), Some(None));
    let past = alloc::vec![b' '; MAX_VALIDATE_LEN + 1];
    assert_eq!(
        validate_document(&mut sandbox(), Format::SystemConfig, &past),
        Err(SyntaxFailure::TooLarge)
    );
}

#[test]
fn a_diagnostic_off_the_document_or_saying_nothing_fails_the_reply() {
    let reply = |line: u32, severity: u8, message: &str| {
        let mut w = Writer::new();
        w.u8(super::REPLY_VALIDATED);
        w.u32(1);
        w.u32(line);
        w.u8(severity);
        w.str(message);
        w.finish()
    };
    let text = b"one\ntwo\n";
    for bad in [
        reply(4, 0, "past the end"),
        reply(1, 9, "no such severity"),
        reply(1, 0, ""),
        reply(1, 0, "a\x1b[2J"),
    ] {
        assert_eq!(
            validate_document(&mut scripted(bad), Format::SystemConfig, text),
            Err(SyntaxFailure::ReplyMalformed)
        );
    }
    let good = validate_document(
        &mut scripted(reply(3, 1, "ignored")),
        Format::SystemConfig,
        text,
    )
    .expect("believed");
    assert_eq!(good[0].line, Some(3));
    assert_eq!(good[0].severity, Severity::Warning);
    let mut many = Writer::new();
    many.u8(super::REPLY_VALIDATED);
    many.u32(u32::try_from(MAX_DIAGNOSTICS + 1).expect("fits"));
    assert_eq!(
        validate_document(&mut scripted(many.finish()), Format::SystemConfig, text),
        Err(SyntaxFailure::ReplyMalformed)
    );
}

#[test]
fn detection_names_a_format_or_none_and_refuses_what_it_cannot_believe() {
    assert_eq!(
        detect(&mut sandbox(), b"#!/bin/sh\n"),
        Ok(Some(Format::Shell))
    );
    assert_eq!(detect(&mut sandbox(), b"plain"), Ok(None));
    assert_eq!(
        detect(&mut sandbox(), &alloc::vec![b'x'; MAX_HEAD_LEN + 1]),
        Err(SyntaxFailure::TooLarge)
    );
    let unknown = alloc::vec![super::REPLY_DETECTED, 99];
    assert_eq!(
        detect(&mut scripted(unknown), b"x"),
        Err(SyntaxFailure::ReplyMalformed)
    );
}

#[test]
fn the_service_refuses_malformed_requests_without_failing() {
    let mut service = TextSyntaxService;
    for request in [
        alloc::vec![],
        alloc::vec![77],
        alloc::vec![super::OP_LEX, 200],
        alloc::vec![super::OP_VALIDATE, 0, 5, 0, 0, 0],
        alloc::vec![super::OP_DETECT, 1, 0, 0, 0, b'x', b'y'],
    ] {
        assert_eq!(
            service.handle(&request),
            [super::REPLY_REFUSED],
            "{request:?}"
        );
    }
}

/// A failure reads as a reason, for the diagnostics that state it.
#[test]
fn a_syntax_failure_says_why() {
    assert_eq!(
        alloc::format!("{}", SyntaxFailure::TooLarge),
        "the request is larger than a parser takes"
    );
    assert_eq!(
        alloc::format!("{}", SyntaxFailure::ReplyMalformed),
        "the parser's answer could not be believed"
    );
}
