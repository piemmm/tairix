//! The markup languages: HTML, and XML.
//!
//! HTML's `<script>` and `<style>` bodies are classified by the JavaScript
//! and CSS lexers, whose state rides in the upper bits of this one's.

use tairix_theme::SyntaxRole;

use crate::lex::{at, find, find_ignoring_case, skip_space, starts_at, Emit, LineLexer, LineState};

/// Which markup language.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Dialect {
    /// HTML: element names ignore case, and scripts and style sheets embed.
    Html,
    /// XML, SVG included.
    Xml,
}

/// What a line was left inside of: the state word's low byte.
const TEXT: u32 = 0;
const COMMENT: u32 = 1;
const TAG: u32 = 2;
const VALUE_DOUBLE: u32 = 3;
const VALUE_SINGLE: u32 = 4;
const CDATA: u32 = 5;
const DECLARATION: u32 = 6;
const INSTRUCTION: u32 = 7;
const SCRIPT: u32 = 8;
const STYLE: u32 = 9;

/// Tag flags, above the low byte while a tag or its attribute value is
/// open: the element's body is a script, a style sheet, or this is an end
/// tag.
const OPENS_SCRIPT: u32 = 1 << 8;
const OPENS_STYLE: u32 = 1 << 9;
const END_TAG: u32 = 1 << 10;

/// The largest embedded-lexer state word this one can carry above its own
/// low byte.
const EMBEDDED_MAX: u32 = 0x00ff_ffff;

/// Classify one line of markup.
pub(crate) fn lex(
    dialect: Dialect,
    state: LineState,
    line: &[u8],
    out: &mut Emit<'_>,
) -> LineState {
    let (mut kind, mut flags) = (state.raw() & 0xff, state.raw() & !0xff);
    let embeds = dialect == Dialect::Html;
    if kind > STYLE || (!embeds && matches!(kind, SCRIPT | STYLE)) {
        (kind, flags) = (TEXT, 0);
    }
    let mut i = 0;
    loop {
        match kind {
            COMMENT => {
                let Some(close) = find(line, i, b"-->") else {
                    out.push(i, line.len(), SyntaxRole::Comment);
                    return LineState::from_raw(COMMENT);
                };
                out.push(i, close + 3, SyntaxRole::Comment);
                (i, kind) = (close + 3, TEXT);
            }
            CDATA => {
                let Some(close) = find(line, i, b"]]>") else {
                    out.push(i, line.len(), SyntaxRole::String);
                    return LineState::from_raw(CDATA);
                };
                out.push(i, close, SyntaxRole::String);
                out.push(close, close + 3, SyntaxRole::Directive);
                (i, kind) = (close + 3, TEXT);
            }
            DECLARATION | INSTRUCTION => {
                let close: &[u8] = if kind == DECLARATION { b">" } else { b"?>" };
                let Some(end) = find(line, i, close) else {
                    out.push(i, line.len(), SyntaxRole::Directive);
                    return LineState::from_raw(kind);
                };
                out.push(i, end + close.len(), SyntaxRole::Directive);
                (i, kind) = (end + close.len(), TEXT);
            }
            VALUE_DOUBLE | VALUE_SINGLE => {
                let quote = if kind == VALUE_DOUBLE { b'"' } else { b'\'' };
                let Some(close) = find(line, i, &[quote]) else {
                    out.push(i, line.len(), SyntaxRole::String);
                    return LineState::from_raw(kind | flags);
                };
                out.push(i, close + 1, SyntaxRole::String);
                (i, kind) = (close + 1, TAG);
            }
            TAG => {
                let (next, next_kind) = match tag(line, i, flags, out) {
                    Ok(resumed) => resumed,
                    Err(open) => return open,
                };
                i = next;
                kind = next_kind;
                if kind != TAG && kind != VALUE_DOUBLE && kind != VALUE_SINGLE {
                    flags = 0;
                }
            }
            SCRIPT | STYLE => {
                let (closer, inner): (&[u8], LineLexer) = if kind == SCRIPT {
                    (b"</script", |s, l, o| {
                        crate::clike::lex(&crate::clike::JAVASCRIPT, s, l, o)
                    })
                } else {
                    (b"</style", crate::css::lex)
                };
                let inner_state = LineState::from_raw(flags >> 8);
                let Some(close) = find_ignoring_case(line, i, closer) else {
                    let next = inner(inner_state, &line[i..], &mut out.from(i));
                    let carried = if next.raw() > EMBEDDED_MAX {
                        0
                    } else {
                        next.raw()
                    };
                    return LineState::from_raw(kind | (carried << 8));
                };
                inner(inner_state, &line[i..close], &mut out.from(i));
                (i, kind, flags) = (close, TEXT, 0);
            }
            _ => match text(dialect, line, i, out) {
                Some((next, next_kind, next_flags)) => {
                    (i, kind, flags) = (next, next_kind, next_flags);
                }
                None => return LineState::START,
            },
        }
    }
}

/// Classify text from `i` up to the next construct, answering where that
/// construct's body starts, its kind and its flags; `None` at the end of
/// the line.
fn text(
    dialect: Dialect,
    line: &[u8],
    mut i: usize,
    out: &mut Emit<'_>,
) -> Option<(usize, u32, u32)> {
    while i < line.len() {
        match line[i] {
            b'<' if starts_at(line, i, b"<!--") => {
                out.push(i, i + 4, SyntaxRole::Comment);
                return Some((i + 4, COMMENT, 0));
            }
            b'<' if starts_at(line, i, b"<![CDATA[") => {
                out.push(i, i + 9, SyntaxRole::Directive);
                return Some((i + 9, CDATA, 0));
            }
            b'<' if at(line, i + 1) == b'!' => return Some((i, DECLARATION, 0)),
            b'<' if at(line, i + 1) == b'?' => return Some((i, INSTRUCTION, 0)),
            b'<' if at(line, i + 1) == b'/' && is_name_start(at(line, i + 2)) => {
                out.push(i, i + 2, SyntaxRole::Punctuation);
                let end = name_end(line, i + 2);
                out.push(i + 2, end, SyntaxRole::Tag);
                return Some((end, TAG, END_TAG));
            }
            b'<' if is_name_start(at(line, i + 1)) => {
                out.push(i, i + 1, SyntaxRole::Punctuation);
                let end = name_end(line, i + 1);
                out.push(i + 1, end, SyntaxRole::Tag);
                let name = &line[i + 1..end];
                let flags = match dialect {
                    Dialect::Html if name.eq_ignore_ascii_case(b"script") => OPENS_SCRIPT,
                    Dialect::Html if name.eq_ignore_ascii_case(b"style") => OPENS_STYLE,
                    _ => 0,
                };
                return Some((end, TAG, flags));
            }
            b'&' => {
                let end = entity_end(line, i);
                if end > i + 1 {
                    out.push(i, end, SyntaxRole::Escape);
                }
                i = end.max(i + 1);
            }
            _ => i += 1,
        }
    }
    None
}

/// Classify a tag's attributes from `i`, answering where the tag's content
/// resumes and what it is, or the state an attribute value left open.
fn tag(
    line: &[u8],
    mut i: usize,
    flags: u32,
    out: &mut Emit<'_>,
) -> Result<(usize, u32), LineState> {
    let mut after_equals = false;
    while i < line.len() {
        match line[i] {
            b' ' | b'\t' | b'\r' => i = skip_space(line, i),
            b'/' if at(line, i + 1) == b'>' => {
                out.push(i, i + 2, SyntaxRole::Punctuation);
                return Ok((i + 2, TEXT));
            }
            b'>' => {
                out.push(i, i + 1, SyntaxRole::Punctuation);
                let body = if flags & END_TAG != 0 {
                    TEXT
                } else if flags & OPENS_SCRIPT != 0 {
                    SCRIPT
                } else if flags & OPENS_STYLE != 0 {
                    STYLE
                } else {
                    TEXT
                };
                return Ok((i + 1, body));
            }
            b'?' if at(line, i + 1) == b'>' => {
                out.push(i, i + 2, SyntaxRole::Directive);
                return Ok((i + 2, TEXT));
            }
            b'=' => {
                out.push(i, i + 1, SyntaxRole::Punctuation);
                after_equals = true;
                i += 1;
            }
            quote @ (b'"' | b'\'') => {
                let Some(close) = find(line, i + 1, &[quote]) else {
                    out.push(i, line.len(), SyntaxRole::String);
                    let open = if quote == b'"' {
                        VALUE_DOUBLE
                    } else {
                        VALUE_SINGLE
                    };
                    return Err(LineState::from_raw(open | flags));
                };
                out.push(i, close + 1, SyntaxRole::String);
                i = close + 1;
                after_equals = false;
            }
            b'<' => return Ok((i, TEXT)),
            _ => {
                let mut end = i + 1;
                while end < line.len()
                    && !matches!(
                        line[end],
                        b' ' | b'\t' | b'\r' | b'=' | b'>' | b'"' | b'\'' | b'<'
                    )
                    && !(line[end] == b'/' && at(line, end + 1) == b'>')
                {
                    end += 1;
                }
                let role = if after_equals {
                    SyntaxRole::String
                } else {
                    SyntaxRole::Attribute
                };
                out.push(i, end, role);
                after_equals = false;
                i = end;
            }
        }
    }
    Err(LineState::from_raw(TAG | flags))
}

/// Whether `byte` may begin an element name.
fn is_name_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b':') || byte >= 0x80
}

/// Where the element name starting at `from` ends.
fn name_end(line: &[u8], from: usize) -> usize {
    let mut end = from;
    while end < line.len()
        && (line[end].is_ascii_alphanumeric()
            || matches!(line[end], b'_' | b':' | b'-' | b'.')
            || line[end] >= 0x80)
    {
        end += 1;
    }
    end
}

/// Where the character reference at the `&` at `from` ends: `&name;`,
/// `&#123;` or `&#x7b;`; `from + 1` when the ampersand opens none.
pub(crate) fn entity_end(line: &[u8], from: usize) -> usize {
    let mut end = from + 1;
    if at(line, end) == b'#' {
        end += 1;
        if matches!(at(line, end), b'x' | b'X') {
            end += 1;
        }
    }
    let body = end;
    while end < line.len() && end - body < 32 && line[end].is_ascii_alphanumeric() {
        end += 1;
    }
    if end > body && at(line, end) == b';' {
        end + 1
    } else {
        from + 1
    }
}
