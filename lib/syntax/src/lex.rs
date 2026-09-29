//! One line at a time: the span vocabulary, the dispatcher, and the byte
//! scanners every lexer shares.
//!
//! A line is untrusted, so every lexer costs time linear in its length: a
//! scan either consumes what it reads or is remembered, and an opener that
//! finds no closer never makes the next one rescan the line.

use alloc::vec::Vec;

use tairix_theme::SyntaxRole;

use crate::Format;

/// Most bytes of one line a lexer reads; the rest of a longer line is left
/// unclassified.
///
/// A containment bound on the work one line can demand, not a capacity: a
/// line this long is a minified bundle or a data dump, not something a
/// person reads for its colours.
pub const MAX_LEX_LINE: usize = 16 * 1024;

/// A classified run of one line's bytes, by byte offset within the line.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Span {
    /// The first byte of the run.
    pub start: u32,
    /// One past the last byte of the run.
    pub end: u32,
    /// What the run is.
    pub role: SyntaxRole,
}

/// Where a line starts, as the lexer left the line before it.
///
/// Opaque outside its format's lexer: every lexer reads a word it does not
/// recognise as the start of a document, so any value is safe to pass back.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct LineState(u32);

impl LineState {
    /// The first line of a document.
    pub const START: Self = Self(0);

    /// The state a wire word names.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// The wire word for this state.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// Classify the first [`MAX_LEX_LINE`] bytes of `line`, which starts in
/// `state`, appending its spans to `spans`, and answer the state the next
/// line starts in.
///
/// The appended spans are ascending, non-overlapping, non-empty, inside the
/// line, and never [`SyntaxRole::Plain`]: a gap between spans is plain text.
pub fn lex_line(format: Format, state: LineState, line: &[u8], spans: &mut Vec<Span>) -> LineState {
    let line = &line[..line.len().min(MAX_LEX_LINE)];
    let mut out = Emit::new(spans);
    match format {
        Format::PlainText => LineState::START,
        Format::Html => crate::markup::lex(crate::markup::Dialect::Html, state, line, &mut out),
        Format::Xml => crate::markup::lex(crate::markup::Dialect::Xml, state, line, &mut out),
        Format::Css => crate::css::lex(state, line, &mut out),
        Format::JavaScript => crate::clike::lex(&crate::clike::JAVASCRIPT, state, line, &mut out),
        Format::Rust => crate::clike::lex(&crate::clike::RUST, state, line, &mut out),
        Format::C => crate::clike::lex(&crate::clike::C, state, line, &mut out),
        Format::Java => crate::clike::lex(&crate::clike::JAVA, state, line, &mut out),
        Format::Json => crate::data::json(line, &mut out),
        Format::Yaml => crate::data::yaml(state, line, &mut out),
        Format::Toml => crate::data::toml(state, line, &mut out),
        Format::Markdown => crate::markdown::lex(state, line, &mut out),
        Format::Python => crate::script::python(state, line, &mut out),
        Format::Shell => crate::script::shell(state, line, &mut out),
        Format::AppSettings | Format::ProgramLibrary => crate::stores::appconf(line, &mut out),
        Format::SystemConfig => crate::stores::system_config(line, &mut out),
        Format::NetworkConfig => crate::stores::network_config(line, &mut out),
        Format::ServiceOverrides => crate::stores::service_overrides(line, &mut out),
        Format::UsersDb => crate::stores::users(state, line, &mut out),
        Format::GroupsDb => crate::stores::groups(state, line, &mut out),
        Format::FontFamily => crate::stores::font_family(line, &mut out),
    }
}

/// One lexer's line step: what an embedding lexer hands a slice to.
pub(crate) type LineLexer = fn(LineState, &[u8], &mut Emit<'_>) -> LineState;

/// Where a lexer's spans go: the one place the output contract is kept.
pub(crate) struct Emit<'a> {
    spans: &'a mut Vec<Span>,
    /// Where this line's spans begin in `spans`; anything before belongs to
    /// another line and is never merged with.
    first: usize,
    /// Added to every offset pushed: where the slice a lexer was handed
    /// starts in the line, for a language embedded in another.
    base: usize,
}

impl<'a> Emit<'a> {
    fn new(spans: &'a mut Vec<Span>) -> Self {
        let first = spans.len();
        Self {
            spans,
            first,
            base: 0,
        }
    }

    /// An emitter for the part of the line starting at `from`, whose lexer
    /// counts offsets from there.
    pub(crate) fn from(&mut self, from: usize) -> Emit<'_> {
        Emit {
            spans: self.spans,
            first: self.first,
            base: self.base + from,
        }
    }

    /// Classify `start..end` as `role`, merging with an abutting run of the
    /// same role. A run that would overlap one already emitted keeps only
    /// what lies past it, so no lexer can break the contract.
    pub(crate) fn push(&mut self, start: usize, end: usize, role: SyntaxRole) {
        if role == SyntaxRole::Plain {
            return;
        }
        let (Ok(mut start), Ok(end)) = (
            u32::try_from(start + self.base),
            u32::try_from(end + self.base),
        ) else {
            return;
        };
        if let Some(last) = self
            .spans
            .get_mut(self.first..)
            .and_then(<[Span]>::last_mut)
        {
            start = start.max(last.end);
            if start >= end {
                return;
            }
            if last.role == role && last.end == start {
                last.end = end;
                return;
            }
        } else if start >= end {
            return;
        }
        self.spans.push(Span { start, end, role });
    }
}

/// Whether `byte` may begin an identifier. Any byte of a multi-byte UTF-8
/// sequence does, so a non-ASCII word stays one word.
pub(crate) const fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80
}

/// Whether `byte` may continue an identifier.
pub(crate) const fn is_ident_continue(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}

/// Where the identifier starting at `from` ends.
pub(crate) fn ident_end(line: &[u8], from: usize) -> usize {
    line.get(from..)
        .and_then(|rest| rest.iter().position(|&b| !is_ident_continue(b)))
        .map_or(line.len(), |len| from + len)
}

/// The first byte at or after `from` that is not a space or a tab.
pub(crate) fn skip_space(line: &[u8], from: usize) -> usize {
    line.get(from..)
        .and_then(|rest| {
            rest.iter()
                .position(|&b| !matches!(b, b' ' | b'\t' | b'\r' | 0x0b | 0x0c))
        })
        .map_or(line.len(), |len| from + len)
}

/// The byte at `at`, or `0` past the end — never a byte any lexer acts on.
pub(crate) fn at(line: &[u8], at: usize) -> u8 {
    line.get(at).copied().unwrap_or(0)
}

/// Whether `line` holds `prefix` starting at `from`.
pub(crate) fn starts_at(line: &[u8], from: usize, prefix: &[u8]) -> bool {
    line.get(from..)
        .is_some_and(|rest| rest.starts_with(prefix))
}

/// The first occurrence of `needle` at or after `from`.
pub(crate) fn find(line: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    let rest = line.get(from..)?;
    let at = match needle {
        [] => None,
        [byte] => rest.iter().position(|b| b == byte),
        _ => rest
            .windows(needle.len())
            .position(|window| window == needle),
    }?;
    Some(from + at)
}

/// The next occurrence of one byte in one line, asked for at positions that
/// do not decrease: each byte is looked at once however often it is asked,
/// so openers that find no closer cannot each rescan the line.
pub(crate) struct NextByte {
    byte: u8,
    /// No occurrence lies in `asked..next`, and `next` is one, or the end of
    /// the line.
    asked: usize,
    next: usize,
}

impl NextByte {
    pub(crate) const fn new(byte: u8) -> Self {
        Self {
            byte,
            asked: usize::MAX,
            next: 0,
        }
    }

    /// The first occurrence at or after `from`.
    pub(crate) fn find(&mut self, line: &[u8], from: usize) -> Option<usize> {
        if !(self.asked..=self.next).contains(&from) {
            self.asked = from;
            self.next = find(line, from, &[self.byte]).unwrap_or(line.len());
        }
        (self.next < line.len()).then_some(self.next)
    }
}

/// Where the numeric literal starting at `from` ends: digits, radix and
/// suffix letters, digit separators, a fraction whose `.` is followed by a
/// digit, and a signed decimal exponent.
pub(crate) fn number_end(line: &[u8], from: usize) -> usize {
    let hex = starts_at(line, from, b"0x") || starts_at(line, from, b"0X");
    let mut end = from;
    while end < line.len() {
        let byte = line[end];
        let digit = byte.is_ascii_alphanumeric() || byte == b'_';
        let fraction = byte == b'.' && at(line, end + 1).is_ascii_digit();
        let exponent_sign = matches!(byte, b'+' | b'-')
            && !hex
            && matches!(at(line, end.wrapping_sub(1)), b'e' | b'E')
            && at(line, end + 1).is_ascii_digit();
        if !(digit || fraction || exponent_sign) {
            break;
        }
        end += 1;
    }
    end
}

/// Whether `word` is in the [`ascending`] `table`.
pub(crate) fn listed(table: &[&str], word: &[u8]) -> bool {
    table
        .binary_search_by(|entry| entry.as_bytes().cmp(word))
        .is_ok()
}

/// Whether `table` ascends strictly byte by byte, as [`listed`] needs.
pub(crate) const fn ascending(table: &[&str]) -> bool {
    let mut at = 1;
    while at < table.len() {
        if !precedes(table[at - 1].as_bytes(), table[at].as_bytes()) {
            return false;
        }
        at += 1;
    }
    true
}

/// Whether `a` sorts strictly before `b`.
const fn precedes(a: &[u8], b: &[u8]) -> bool {
    let mut at = 0;
    while at < a.len() && at < b.len() {
        if a[at] != b[at] {
            return a[at] < b[at];
        }
        at += 1;
    }
    a.len() < b.len()
}

/// How many bytes the UTF-8 sequence led by `lead` occupies (one for a
/// byte no sequence starts with).
pub(crate) const fn utf8_len(lead: u8) -> usize {
    match lead {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// Where the escape sequence starting at the backslash `from` ends: the
/// backslash and the one character it escapes, or a `\x`/`\u` hex run.
pub(crate) fn escape_end(line: &[u8], from: usize) -> usize {
    let after = from + 1;
    match at(line, after) {
        b'x' => hex_run(line, after + 1, 2),
        b'u' if at(line, after + 1) == b'{' => braced_end(line, after + 2),
        b'u' => hex_run(line, after + 1, 4),
        b'U' => hex_run(line, after + 1, 8),
        0 => line.len().min(after),
        lead => (after + utf8_len(lead)).min(line.len()),
    }
}

/// Where a `\u{…}` body starting at `from` ends: at most six hex digits,
/// with the underscores Rust lets follow each, and the closing brace when it
/// is there. Nothing else is taken, so an unclosed one never swallows the
/// quote that ends its string.
fn braced_end(line: &[u8], from: usize) -> usize {
    let mut end = from;
    let mut digits = 0;
    loop {
        match at(line, end) {
            b'_' => end += 1,
            byte if byte.is_ascii_hexdigit() && digits < 6 => {
                digits += 1;
                end += 1;
            }
            b'}' => return end + 1,
            _ => return end,
        }
    }
}

/// Up to `most` hex digits from `from`.
fn hex_run(line: &[u8], from: usize, most: usize) -> usize {
    let mut end = from;
    while end < line.len() && end - from < most && line[end].is_ascii_hexdigit() {
        end += 1;
    }
    end
}

/// Whether `line` ends with a backslash continuing it onto the next.
pub(crate) fn continues(line: &[u8]) -> bool {
    line.last() == Some(&b'\\')
}

/// Where the quoted body starting at `from` closes at `quote`, skipping a
/// backslash escape when `escapes`, and whether it closed on this line.
pub(crate) fn string_end(line: &[u8], from: usize, quote: u8, escapes: bool) -> (usize, bool) {
    let mut i = from;
    while i < line.len() {
        match line[i] {
            b'\\' if escapes => i = escape_end(line, i),
            byte if byte == quote => return (i + 1, true),
            _ => i += 1,
        }
    }
    (line.len(), false)
}

/// The first occurrence of the ASCII `needle` at or after `from`, ignoring
/// ASCII case.
pub(crate) fn find_ignoring_case(line: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    let rest = line.get(from..)?;
    if needle.is_empty() || needle.len() > rest.len() {
        return None;
    }
    rest.windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
        .map(|at| from + at)
}
