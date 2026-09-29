//! The sandboxed text-syntax service: colouring, detection and store
//! validation of a document an editor holds.
//!
//! A document is untrusted input, so the editor holding it never lexes or
//! validates it: it sends lines to a capability-empty worker that runs
//! `tairix_syntax`, and believes nothing the worker answers until it has
//! checked it — every span in bounds, ascending, non-overlapping and of a
//! real role, every diagnostic on a line the document has. A reply that
//! fails those checks retires the worker.
//!
//! The requests carry the document's own bytes, which the editor already
//! holds in the clear, to a worker that can pass them nowhere; its memory
//! reaches no other process, because the kernel zeroes a frame before it is
//! reused.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_syntax::{
    format_for_head, lex_line, validate, Diagnostic, Format, LineState, Severity, Span,
    MAX_DIAGNOSTICS, MAX_LEX_LINE, MAX_STORE_LEN,
};
use tairix_theme::SyntaxRole;

use crate::host::{Launcher, ParserSandbox, SandboxError, Unbelieved};
use crate::proto::MAX_FRAME;
use crate::wire::{Reader, Writer};
use crate::worker::Service;

/// Most line bytes one lex request carries.
///
/// A bound on the untrusted reply, not a capacity: a span is at least one
/// byte and encodes in nine, so a reply to this many bytes stays well inside
/// [`crate::MAX_FRAME`]. A batch past it is split by the caller.
pub const MAX_LEX_BATCH_BYTES: usize = 512 * 1024;

/// Most lines one lex request carries.
pub const MAX_LEX_BATCH_LINES: usize = 4096;

/// Largest document a validation request carries: one byte past the
/// longest store, so a store's own parser is what refuses an over-long one.
pub const MAX_VALIDATE_LEN: usize = MAX_STORE_LEN + 1;

/// Longest diagnostic message, in bytes.
pub const MAX_MESSAGE_LEN: usize = 256;

/// Most of a document's head a detection request carries.
pub const MAX_HEAD_LEN: usize = 4096;

/// Encoded sizes: a span, a lexed line's header, and a diagnostic.
const SPAN_WIRE: usize = 4 + 4 + 1;
const LINE_WIRE: usize = 4 + 4;
const DIAGNOSTIC_WIRE: usize = 4 + 1 + 4 + MAX_MESSAGE_LEN;

// Every request, and the largest reply it can draw, fits one frame.
const _: () = {
    assert!(10 + MAX_LEX_BATCH_LINES * 4 + MAX_LEX_BATCH_BYTES <= MAX_FRAME);
    assert!(5 + MAX_LEX_BATCH_LINES * LINE_WIRE + MAX_LEX_BATCH_BYTES * SPAN_WIRE <= MAX_FRAME);
    assert!(6 + MAX_VALIDATE_LEN <= MAX_FRAME);
    assert!(5 + MAX_DIAGNOSTICS * DIAGNOSTIC_WIRE <= MAX_FRAME);
};

/// Request opcodes.
const OP_LEX: u8 = 1;
const OP_VALIDATE: u8 = 2;
const OP_DETECT: u8 = 3;

/// Reply tags.
const REPLY_REFUSED: u8 = 0;
const REPLY_LEXED: u8 = 1;
const REPLY_VALIDATED: u8 = 2;
const REPLY_DETECTED: u8 = 3;

/// A detection that names no format.
const NO_FORMAT: u8 = u8::MAX;

/// Severity wire codes.
const SEVERITY_ERROR: u8 = 0;
const SEVERITY_WARNING: u8 = 1;

/// What the service can fail with, as the caller sees it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SyntaxFailure {
    /// The sandbox itself failed: a crash, a launch failure, an oversize
    /// frame.
    Sandbox(SandboxError),
    /// The worker refused the request as malformed.
    Refused,
    /// The request is past one of this protocol's bounds; nothing was sent.
    TooLarge,
    /// The worker's reply broke the reply grammar or one of its invariants:
    /// it cannot be believed.
    ReplyMalformed,
}

impl Unbelieved for SyntaxFailure {
    fn unbelieved(&self) -> bool {
        *self == Self::ReplyMalformed
    }
}

/// One lexed batch: every line's spans in one buffer, and where each line's
/// spans end and the state its successor starts in.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LexedBatch {
    /// Every line's spans, line after line.
    pub spans: Vec<Span>,
    /// For each line: one past its last span in `spans`, and the state the
    /// next line starts in.
    pub lines: Vec<(usize, LineState)>,
}

impl LexedBatch {
    /// The spans of the batch's line `index`.
    #[must_use]
    pub fn line(&self, index: usize) -> &[Span] {
        let end = self.lines.get(index).map_or(0, |(end, _)| *end);
        let start = index
            .checked_sub(1)
            .and_then(|previous| self.lines.get(previous))
            .map_or(0, |(end, _)| *end);
        self.spans.get(start..end).unwrap_or(&[])
    }
}

/// The worker side: colour, validate and detect, total over any request.
#[derive(Debug, Default)]
pub struct TextSyntaxService;

impl Service for TextSyntaxService {
    fn handle(&mut self, request: &[u8]) -> Vec<u8> {
        dispatch(request).unwrap_or_else(|| {
            let mut w = Writer::new();
            w.u8(REPLY_REFUSED);
            w.finish()
        })
    }
}

/// Decode, serve, and encode one request; `None` for a malformed one.
fn dispatch(request: &[u8]) -> Option<Vec<u8>> {
    let mut r = Reader::new(request);
    let mut w = Writer::new();
    match r.u8().ok()? {
        OP_LEX => {
            let format = Format::from_index(r.u8().ok()?)?;
            let mut state = LineState::from_raw(r.u32().ok()?);
            let count = r.u32().ok()? as usize;
            if count > MAX_LEX_BATCH_LINES {
                return None;
            }
            w.u8(REPLY_LEXED);
            w.u32(u32::try_from(count).ok()?);
            let mut spans = Vec::new();
            let mut total = 0usize;
            for _ in 0..count {
                let line = r.bytes(MAX_LEX_LINE).ok()?;
                total += line.len();
                if total > MAX_LEX_BATCH_BYTES {
                    return None;
                }
                spans.clear();
                state = lex_line(format, state, line, &mut spans);
                w.u32(state.raw());
                w.u32(u32::try_from(spans.len()).ok()?);
                for span in &spans {
                    w.u32(span.start);
                    w.u32(span.end);
                    w.u8(span.role.index());
                }
            }
            r.is_exhausted().then(|| w.finish())
        }
        OP_VALIDATE => {
            let format = Format::from_index(r.u8().ok()?)?;
            let text = r.bytes(MAX_VALIDATE_LEN).ok()?;
            if !r.is_exhausted() {
                return None;
            }
            let diagnostics = validate(format, text);
            w.u8(REPLY_VALIDATED);
            let shown = diagnostics.len().min(MAX_DIAGNOSTICS);
            w.u32(u32::try_from(shown).ok()?);
            for diagnostic in diagnostics.iter().take(shown) {
                w.u32(diagnostic.line.unwrap_or(0));
                w.u8(match diagnostic.severity {
                    Severity::Error => SEVERITY_ERROR,
                    Severity::Warning => SEVERITY_WARNING,
                });
                w.str(clipped(&diagnostic.message));
            }
            Some(w.finish())
        }
        OP_DETECT => {
            let head = r.bytes(MAX_HEAD_LEN).ok()?;
            if !r.is_exhausted() {
                return None;
            }
            w.u8(REPLY_DETECTED);
            w.u8(format_for_head(head).map_or(NO_FORMAT, Format::index));
            Some(w.finish())
        }
        _ => None,
    }
}

/// `message` cut to [`MAX_MESSAGE_LEN`] at a character boundary.
fn clipped(message: &str) -> &str {
    let mut end = message.len().min(MAX_MESSAGE_LEN);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    &message[..end]
}

/// Colour `lines`, the first of which starts in `state`, each in the state
/// the one before left.
///
/// # Errors
///
/// [`SyntaxFailure::TooLarge`] past [`MAX_LEX_BATCH_LINES`] or
/// [`MAX_LEX_BATCH_BYTES`], or a line past [`MAX_LEX_LINE`] (a caller sends
/// only what a lexer reads); otherwise a sandbox failure, a refusal, or a
/// reply that cannot be believed.
pub fn lex_lines<L: Launcher, S: tairix_log::Sink>(
    sandbox: &mut ParserSandbox<L, S>,
    format: Format,
    state: LineState,
    lines: &[&[u8]],
) -> Result<LexedBatch, SyntaxFailure> {
    if lines.len() > MAX_LEX_BATCH_LINES || lines.iter().any(|line| line.len() > MAX_LEX_LINE) {
        return Err(SyntaxFailure::TooLarge);
    }
    // Both bounds held, so the sum cannot overflow.
    let total: usize = lines.iter().map(|line| line.len()).sum();
    if total > MAX_LEX_BATCH_BYTES {
        return Err(SyntaxFailure::TooLarge);
    }
    let mut w = Writer::with_capacity(10 + lines.len() * 4 + total);
    w.u8(OP_LEX);
    w.u8(format.index());
    w.u32(state.raw());
    w.u32(u32::try_from(lines.len()).map_err(|_| SyntaxFailure::TooLarge)?);
    for line in lines {
        w.bytes(line);
    }
    let request = w.finish();
    sandbox.ask(|sandbox| {
        let reply = sandbox.request(&request).map_err(SyntaxFailure::Sandbox)?;
        decode_lexed(&reply, lines)
    })
}

/// Believe a lex reply only once every span of every line holds the span
/// contract against the line the caller sent.
fn decode_lexed(reply: &[u8], lines: &[&[u8]]) -> Result<LexedBatch, SyntaxFailure> {
    let bad = |_| SyntaxFailure::ReplyMalformed;
    let mut r = Reader::new(reply);
    match r.u8().map_err(bad)? {
        REPLY_LEXED => {}
        REPLY_REFUSED if r.is_exhausted() => return Err(SyntaxFailure::Refused),
        _ => return Err(SyntaxFailure::ReplyMalformed),
    }
    if r.u32().map_err(bad)? as usize != lines.len() {
        return Err(SyntaxFailure::ReplyMalformed);
    }
    let mut batch = LexedBatch {
        spans: Vec::new(),
        lines: Vec::with_capacity(lines.len()),
    };
    for line in lines {
        let next = LineState::from_raw(r.u32().map_err(bad)?);
        let count = r.u32().map_err(bad)? as usize;
        if count > line.len() {
            return Err(SyntaxFailure::ReplyMalformed);
        }
        let limit = line.len().min(MAX_LEX_LINE);
        let mut last = 0usize;
        for _ in 0..count {
            let start = r.u32().map_err(bad)?;
            let end = r.u32().map_err(bad)?;
            let role = SyntaxRole::from_index(r.u8().map_err(bad)?)
                .ok_or(SyntaxFailure::ReplyMalformed)?;
            let ordered = (start as usize) >= last && start < end && (end as usize) <= limit;
            if !ordered || role == SyntaxRole::Plain {
                return Err(SyntaxFailure::ReplyMalformed);
            }
            last = end as usize;
            batch.spans.push(Span { start, end, role });
        }
        batch.lines.push((batch.spans.len(), next));
    }
    if r.is_exhausted() {
        Ok(batch)
    } else {
        Err(SyntaxFailure::ReplyMalformed)
    }
}

/// Validate `text` as a store of `format`.
///
/// # Errors
///
/// [`SyntaxFailure::TooLarge`] past [`MAX_VALIDATE_LEN`]; otherwise a
/// sandbox failure, a refusal, or a reply that cannot be believed.
pub fn validate_document<L: Launcher, S: tairix_log::Sink>(
    sandbox: &mut ParserSandbox<L, S>,
    format: Format,
    text: &[u8],
) -> Result<Vec<Diagnostic>, SyntaxFailure> {
    if text.len() > MAX_VALIDATE_LEN {
        return Err(SyntaxFailure::TooLarge);
    }
    let mut w = Writer::with_capacity(6 + text.len());
    w.u8(OP_VALIDATE);
    w.u8(format.index());
    w.bytes(text);
    let request = w.finish();
    let lines = text.split(|&b| b == b'\n').count();
    sandbox.ask(|sandbox| {
        let reply = sandbox.request(&request).map_err(SyntaxFailure::Sandbox)?;
        decode_validated(&reply, lines)
    })
}

/// Believe a validation reply only once every diagnostic lies on a line of
/// a `lines`-line document and says something.
fn decode_validated(reply: &[u8], lines: usize) -> Result<Vec<Diagnostic>, SyntaxFailure> {
    let bad = |_| SyntaxFailure::ReplyMalformed;
    let mut r = Reader::new(reply);
    match r.u8().map_err(bad)? {
        REPLY_VALIDATED => {}
        REPLY_REFUSED if r.is_exhausted() => return Err(SyntaxFailure::Refused),
        _ => return Err(SyntaxFailure::ReplyMalformed),
    }
    let count = r.u32().map_err(bad)? as usize;
    if count > MAX_DIAGNOSTICS {
        return Err(SyntaxFailure::ReplyMalformed);
    }
    let mut diagnostics = Vec::with_capacity(count);
    for _ in 0..count {
        let line = r.u32().map_err(bad)?;
        let severity = match r.u8().map_err(bad)? {
            SEVERITY_ERROR => Severity::Error,
            SEVERITY_WARNING => Severity::Warning,
            _ => return Err(SyntaxFailure::ReplyMalformed),
        };
        let message: String = r.string(MAX_MESSAGE_LEN).map_err(bad)?;
        if message.is_empty() || line as usize > lines || message.chars().any(char::is_control) {
            return Err(SyntaxFailure::ReplyMalformed);
        }
        diagnostics.push(Diagnostic {
            line: (line > 0).then_some(line),
            severity,
            message,
        });
    }
    if r.is_exhausted() {
        Ok(diagnostics)
    } else {
        Err(SyntaxFailure::ReplyMalformed)
    }
}

/// The format a document's head names, if it names one.
///
/// # Errors
///
/// [`SyntaxFailure::TooLarge`] past [`MAX_HEAD_LEN`]; otherwise a sandbox
/// failure, a refusal, or a reply that cannot be believed.
pub fn detect<L: Launcher, S: tairix_log::Sink>(
    sandbox: &mut ParserSandbox<L, S>,
    head: &[u8],
) -> Result<Option<Format>, SyntaxFailure> {
    if head.len() > MAX_HEAD_LEN {
        return Err(SyntaxFailure::TooLarge);
    }
    let mut w = Writer::with_capacity(5 + head.len());
    w.u8(OP_DETECT);
    w.bytes(head);
    let request = w.finish();
    sandbox.ask(|sandbox| {
        let reply = sandbox.request(&request).map_err(SyntaxFailure::Sandbox)?;
        decode_detected(&reply)
    })
}

/// Believe a detection reply only once it names a format or none.
fn decode_detected(reply: &[u8]) -> Result<Option<Format>, SyntaxFailure> {
    let bad = |_| SyntaxFailure::ReplyMalformed;
    let mut r = Reader::new(reply);
    match r.u8().map_err(bad)? {
        REPLY_DETECTED => {}
        REPLY_REFUSED if r.is_exhausted() => return Err(SyntaxFailure::Refused),
        _ => return Err(SyntaxFailure::ReplyMalformed),
    }
    let code = r.u8().map_err(bad)?;
    if !r.is_exhausted() {
        return Err(SyntaxFailure::ReplyMalformed);
    }
    match code {
        NO_FORMAT => Ok(None),
        index => Format::from_index(index)
            .map(Some)
            .ok_or(SyntaxFailure::ReplyMalformed),
    }
}

#[cfg(test)]
#[path = "textsyntax_tests.rs"]
mod tests;
