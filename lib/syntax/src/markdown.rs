//! Markdown: its block constructs a line at a time, and its inlines.

use tairix_theme::SyntaxRole;

use crate::lex::{at, find, skip_space, Emit, LineState, NextByte};

/// A fenced code block is open: the fence byte above the low byte and its
/// length above that.
const FENCE: u32 = 1;

/// Classify one line of Markdown.
pub(crate) fn lex(state: LineState, line: &[u8], out: &mut Emit<'_>) -> LineState {
    let indent = line.iter().take_while(|&&b| b == b' ').count();
    let block = indent < 4;
    if state.raw() & 0xff == FENCE {
        let fence = ((state.raw() >> 8) & 0xff) as u8;
        let length = (state.raw() >> 16) as usize;
        out.push(0, line.len(), SyntaxRole::Code);
        let run = line[indent..].iter().take_while(|&&b| b == fence).count();
        let closes = block && run >= length && skip_space(line, indent + run) == line.len();
        return if closes { LineState::START } else { state };
    }
    if !block {
        out.push(indent, line.len(), SyntaxRole::Code);
        return LineState::START;
    }
    let rest = indent;
    let lead = at(line, rest);
    if matches!(lead, b'`' | b'~') {
        let run = line[rest..].iter().take_while(|&&b| b == lead).count();
        if run >= 3 && (lead == b'~' || find(line, rest + run, b"`").is_none()) {
            out.push(rest, line.len(), SyntaxRole::Code);
            let length = u32::try_from(run).unwrap_or(u32::MAX).min(0xffff);
            return LineState::from_raw(FENCE | (u32::from(lead) << 8) | (length << 16));
        }
    }
    if lead == b'#' {
        let level = line[rest..].iter().take_while(|&&b| b == b'#').count();
        if level <= 6 && matches!(at(line, rest + level), 0 | b' ' | b'\t') {
            out.push(rest, line.len(), SyntaxRole::Heading);
            return LineState::START;
        }
    }
    if is_rule(&line[rest..]) {
        out.push(rest, line.len(), SyntaxRole::Punctuation);
        return LineState::START;
    }
    let mut i = rest;
    while at(line, i) == b'>' {
        out.push(i, i + 1, SyntaxRole::Punctuation);
        i = skip_space(line, i + 1);
    }
    let marker = list_marker(line, i);
    if marker > i {
        out.push(i, marker, SyntaxRole::Punctuation);
        i = marker;
    }
    inline(line, i, out);
    LineState::START
}

/// Whether `line` is a thematic break: three or more of one of `*`, `-`
/// or `_`, with nothing but spaces between.
fn is_rule(line: &[u8]) -> bool {
    let Some(&mark) = line.iter().find(|&&b| b != b' ') else {
        return false;
    };
    if !matches!(mark, b'*' | b'-' | b'_') {
        return false;
    }
    let mut marks = 0;
    for &byte in line {
        if byte == mark {
            marks += 1;
        } else if byte != b' ' {
            return false;
        }
    }
    marks >= 3
}

/// Where the list marker at `from` ends (its following space included), or
/// `from` when there is none.
fn list_marker(line: &[u8], from: usize) -> usize {
    match at(line, from) {
        b'-' | b'*' | b'+' if matches!(at(line, from + 1), b' ' | b'\t') => from + 2,
        b'0'..=b'9' => {
            let digits = line[from..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
            let after = from + digits;
            if digits <= 9
                && matches!(at(line, after), b'.' | b')')
                && matches!(at(line, after + 1), b' ' | b'\t')
            {
                after + 2
            } else {
                from
            }
        }
        _ => from,
    }
}

/// The longest backtick run that opens a code span; a longer one is text.
/// It keeps the record of where each run length last occurs a fixed size,
/// as the reference Markdown parser (`cmark`) bounds its own.
const MAX_CODE_TICKS: usize = 32;

/// Code spans: one opened by a run of backticks closes at the next run of
/// exactly its length.
#[derive(Default)]
struct CodeSpans {
    /// Once a span has failed to close: where the last run of each length
    /// starts (zero for none), so an opener no closer follows is refused
    /// without a scan.
    last: Option<[usize; MAX_CODE_TICKS + 1]>,
}

impl CodeSpans {
    /// Where the span opened by the `len` backticks ending at `from`
    /// closes, if one does.
    fn close(&mut self, line: &[u8], from: usize, len: usize) -> Option<usize> {
        if len > MAX_CODE_TICKS || self.last.is_some_and(|last| last[len] < from) {
            return None;
        }
        let mut last = [0; MAX_CODE_TICKS + 1];
        for (start, run) in backtick_runs(line, from) {
            if run == len {
                return Some(start);
            }
            if let Some(slot) = last.get_mut(run) {
                *slot = start;
            }
        }
        self.last = Some(last);
        None
    }
}

/// The backtick runs from `from` on: each one's start and length.
fn backtick_runs(line: &[u8], from: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut next = from;
    core::iter::from_fn(move || {
        let start = find(line, next, b"`")?;
        let len = line[start..].iter().take_while(|&&b| b == b'`').count();
        next = start + len;
        Some((start, len))
    })
}

/// The closers inline constructs look ahead for, each found once per line.
struct Closers {
    code: CodeSpans,
    bracket: NextByte,
    paren: NextByte,
    angle: NextByte,
}

/// Classify inline constructs from `from`: code spans, emphasis, links,
/// autolinks, escapes and character references.
fn inline(line: &[u8], from: usize, out: &mut Emit<'_>) {
    let mut closers = Closers {
        code: CodeSpans::default(),
        bracket: NextByte::new(b']'),
        paren: NextByte::new(b')'),
        angle: NextByte::new(b'>'),
    };
    let mut i = from;
    while i < line.len() {
        match line[i] {
            b'`' => {
                let run = line[i..].iter().take_while(|&&b| b == b'`').count();
                match closers.code.close(line, i + run, run) {
                    Some(close) => {
                        out.push(i, close + run, SyntaxRole::Code);
                        i = close + run;
                    }
                    None => i += run,
                }
            }
            b'\\' if at(line, i + 1).is_ascii_punctuation() => {
                out.push(i, i + 2, SyntaxRole::Escape);
                i += 2;
            }
            mark @ (b'*' | b'_') => {
                let run = line[i..].iter().take(2).take_while(|&&b| b == mark).count();
                let opens = !at(line, i + run).is_ascii_whitespace()
                    && at(line, i + run) != 0
                    && (mark == b'*' || !at(line, i.wrapping_sub(1)).is_ascii_alphanumeric());
                let close = if opens {
                    find(line, i + run, &line[i..i + run])
                } else {
                    None
                };
                match close {
                    Some(close) if close > i + run => {
                        out.push(i, close + run, SyntaxRole::Emphasis);
                        i = close + run;
                    }
                    _ => i += run,
                }
            }
            b'!' if at(line, i + 1) == b'[' => i = link(line, i, i + 1, &mut closers, out),
            b'[' => i = link(line, i, i, &mut closers, out),
            b'<' => match angled(line, i, &mut closers.angle) {
                Some((end, role)) => {
                    out.push(i, end, role);
                    i = end;
                }
                None => i += 1,
            },
            b'&' => {
                let end = line[i + 1..]
                    .iter()
                    .take(32)
                    .position(|&b| b == b';')
                    .map_or(i + 1, |semi| i + 1 + semi + 1);
                let named = end > i + 2
                    && line[i + 1..end - 1]
                        .iter()
                        .all(|b| b.is_ascii_alphanumeric() || *b == b'#');
                if named {
                    out.push(i, end, SyntaxRole::Escape);
                    i = end;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
}

/// Classify a link or image whose text opens with the `[` at `bracket`
/// (the construct starting at `start`), answering where it ends.
fn link(
    line: &[u8],
    start: usize,
    bracket: usize,
    closers: &mut Closers,
    out: &mut Emit<'_>,
) -> usize {
    let Some(close) = closers.bracket.find(line, bracket + 1) else {
        return bracket + 1;
    };
    let target = match at(line, close + 1) {
        b'(' => closers.paren.find(line, close + 2),
        b'[' => closers.bracket.find(line, close + 2),
        _ => None,
    };
    let end = target.map_or(close + 1, |target| target + 1);
    out.push(start, end, SyntaxRole::Link);
    end
}

/// The autolink (`<scheme:…>`, `<name@host>`) or inline tag opened by the
/// `<` at `open`: where it ends and what it is.
fn angled(line: &[u8], open: usize, angle: &mut NextByte) -> Option<(usize, SyntaxRole)> {
    let body = open + 1;
    let word = find_any(line, body, b" \t<>").unwrap_or(line.len());
    let text = &line[body..word];
    if at(line, word) == b'>' && text.iter().any(|&b| b == b':' || b == b'@') {
        return Some((word + 1, SyntaxRole::Link));
    }
    if !text
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'/')
    {
        return None;
    }
    angle
        .find(line, word)
        .map(|close| (close + 1, SyntaxRole::Tag))
}

/// The first byte at or after `from` that is one of `set`.
fn find_any(line: &[u8], from: usize, set: &[u8]) -> Option<usize> {
    let at = line.get(from..)?.iter().position(|b| set.contains(b))?;
    Some(from + at)
}
