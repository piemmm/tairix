//! Deterministic fuzz harness for document syntax.
//!
//! Invariants, for any bytes and any state word, in every format:
//!
//! 1. [`lex_line`] never panics, and its spans are ascending,
//!    non-overlapping, non-empty, never plain, and inside both the line and
//!    [`MAX_LEX_LINE`].
//! 2. Lexing is a function: the same line from the same state yields the same
//!    spans and the same next state.
//! 3. A document lexed line by line, each line from the state the one before
//!    it left, holds invariant 1 on every line — which is what drives every
//!    lexer through its multi-line states.
//! 4. [`validate`], [`format_for_head`] and [`store_for_name`] never panic,
//!    and a diagnostic's line lies inside the document.
//!
//! Runs the fixed smoke sweep under plain `cargo test`; keeps drawing from
//! the same seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

use tairix_fuzzseed::Prng;
use tairix_syntax::{
    format_for_head, lex_line, store_for_name, validate, Format, LineState, Span, MAX_LEX_LINE,
};
use tairix_theme::SyntaxRole;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 4_000;

/// Fragments every lexer treats specially, mixed so a generated document
/// opens and closes their multi-line constructs in every order.
const TOKENS: &[&str] = &[
    "/*",
    "*/",
    "//",
    "#",
    "#!",
    "\"",
    "'",
    "`",
    "\"\"\"",
    "'''",
    "r#\"",
    "\"#",
    "<!--",
    "-->",
    "<![CDATA[",
    "]]>",
    "<script>",
    "</script>",
    "<style>",
    "</style>",
    "<",
    ">",
    "</",
    "/>",
    "=",
    ":",
    ";",
    "{",
    "}",
    "[",
    "]",
    "(",
    ")",
    "\\",
    "\\\n",
    "$",
    "${",
    "<<EOF",
    "<<-END",
    "EOF",
    "END",
    "```",
    "~~~",
    "---",
    "- ",
    "* ",
    "**",
    "_",
    "&amp;",
    "&",
    "@media",
    "@x",
    "0x1f",
    "1.5e-3",
    "fn",
    "def",
    "class",
    "let",
    "if",
    "then",
    "fi",
    "true",
    "null",
    "key",
    "a.b",
    "eth0.mtu",
    "os.loginType",
    "text",
    "enabled",
    "tairix-users-v1",
    "tairix-groups-v1",
    "wheel:0",
    "label = x",
    "face",
    " ",
    "  ",
    "\t",
    "\r",
    "é",
    "\u{202e}",
    "\0",
    "\x1b",
];

fn assert_contract(spans: &[Span], len: usize, format: Format) {
    let mut last = 0;
    for span in spans {
        assert!(span.start < span.end, "{format:?}: empty span {span:?}");
        assert!(span.start >= last, "{format:?}: overlap at {span:?}");
        assert!(
            span.end as usize <= len.min(MAX_LEX_LINE),
            "{format:?}: span past the line: {span:?} in {len}"
        );
        assert_ne!(span.role, SyntaxRole::Plain, "{format:?}: a plain span");
        last = span.end;
    }
}

/// A line built from `TOKENS` and random bytes.
fn soup(rng: &mut Prng, out: &mut Vec<u8>) {
    out.clear();
    for _ in 0..rng.at_most(12) {
        if rng.below(4) == 0 {
            out.push(rng.next_u8());
        } else {
            out.extend_from_slice(rng.pick(TOKENS).as_bytes());
        }
    }
}

#[test]
fn any_line_from_any_state_holds_the_span_contract() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_line_from_any_state_holds_the_span_contract",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut line = Vec::new();
    let (mut first, mut again) = (Vec::new(), Vec::new());
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            if rng.below(3) == 0 {
                line.clear();
                line.resize(rng.at_most(96), 0);
                rng.fill(&mut line);
            } else {
                soup(&mut rng, &mut line);
            }
            let state = LineState::from_raw(match rng.below(3) {
                0 => rng.next_u32(),
                1 => rng.next_u32() & 0xffff,
                _ => rng.next_u32() & 0x3ff,
            });
            let format = *rng.pick(&Format::ALL);
            first.clear();
            again.clear();
            let next = lex_line(format, state, &line, &mut first);
            assert_contract(&first, line.len(), format);
            assert_eq!(
                lex_line(format, state, &line, &mut again),
                next,
                "{format:?} is a function"
            );
            assert_eq!(first, again, "{format:?}: the same line lexes the same");
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}

#[test]
fn documents_lexed_line_by_line_hold_the_contract_on_every_line() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "documents_lexed_line_by_line_hold_the_contract_on_every_line",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut line = Vec::new();
    let mut spans = Vec::new();
    loop {
        for _ in 0..SMOKE_ITERATIONS / 8 {
            let format = *rng.pick(&Format::ALL);
            let mut state = LineState::START;
            for _ in 0..rng.at_most(24) {
                soup(&mut rng, &mut line);
                spans.clear();
                state = lex_line(format, state, &line, &mut spans);
                assert_contract(&spans, line.len(), format);
            }
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}

#[test]
fn detection_and_validation_never_panic_and_stay_inside_the_document() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "detection_and_validation_never_panic_and_stay_inside_the_document",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut document = Vec::new();
    let mut line = Vec::new();
    loop {
        for _ in 0..SMOKE_ITERATIONS / 8 {
            document.clear();
            for _ in 0..rng.at_most(16) {
                soup(&mut rng, &mut line);
                document.extend_from_slice(&line);
                document.push(b'\n');
            }
            let _ = format_for_head(&document);
            if let Ok(name) = core::str::from_utf8(&line) {
                let _ = store_for_name(name);
            }
            let lines = document.split(|&b| b == b'\n').count();
            for format in Format::ALL {
                for diagnostic in validate(format, &document) {
                    assert!(
                        !diagnostic.message.is_empty(),
                        "{format:?}: a silent diagnostic"
                    );
                    if let Some(at) = diagnostic.line {
                        assert!(
                            at >= 1 && at as usize <= lines,
                            "{format:?}: line {at} outside a {lines}-line document"
                        );
                    }
                }
            }
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}
