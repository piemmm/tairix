//! The data formats: JSON, YAML and TOML.

use tairix_theme::SyntaxRole;

use crate::clike::quoted;
use crate::lex::{
    at, find, ident_end, is_ident_continue, number_end, skip_space, starts_at, string_end, Emit,
    LineState,
};

/// Classify one line of JSON. Nothing in JSON spans lines.
pub(crate) fn json(line: &[u8], out: &mut Emit<'_>) -> LineState {
    let mut i = 0;
    while i < line.len() {
        match line[i] {
            b' ' | b'\t' | b'\r' => i = skip_space(line, i),
            b'"' => {
                let (end, closed) = string_end(line, i + 1, b'"', true);
                if closed && at(line, skip_space(line, end)) == b':' {
                    out.push(i, end, SyntaxRole::Key);
                } else {
                    out.push(i, i + 1, SyntaxRole::String);
                    quoted(line, i + 1, b'"', out);
                }
                i = end;
            }
            b'-' | b'0'..=b'9' => {
                let end = number_end(line, i + 1);
                out.push(i, end, SyntaxRole::Number);
                i = end;
            }
            b'{' | b'}' | b'[' | b']' | b',' | b':' => {
                out.push(i, i + 1, SyntaxRole::Punctuation);
                i += 1;
            }
            byte if byte.is_ascii_alphabetic() => {
                let end = ident_end(line, i);
                let role = match &line[i..end] {
                    b"true" | b"false" | b"null" => SyntaxRole::Keyword,
                    _ => SyntaxRole::Error,
                };
                out.push(i, end, role);
                i = end;
            }
            b'/' => {
                // JSON has no comments: one here is a document another
                // reader will refuse.
                out.push(i, line.len(), SyntaxRole::Error);
                i = line.len();
            }
            _ => {
                out.push(i, i + 1, SyntaxRole::Error);
                i += 1;
            }
        }
    }
    LineState::START
}

/// A YAML block scalar (`|` or `>`) is open, belonging to a line indented
/// by the column above this tag.
const YAML_BLOCK: u32 = 1;

/// Classify one line of YAML.
pub(crate) fn yaml(state: LineState, line: &[u8], out: &mut Emit<'_>) -> LineState {
    let indent = line.iter().take_while(|&&b| b == b' ').count();
    if state.raw() & 0xff == YAML_BLOCK {
        let parent = (state.raw() >> 8) as usize;
        if indent == line.len() {
            return state;
        }
        if indent > parent {
            out.push(indent, line.len(), SyntaxRole::String);
            return state;
        }
    }
    let mut i = indent;
    if indent == 0
        && (starts_at(line, 0, b"---") || starts_at(line, 0, b"..."))
        && matches!(at(line, 3), 0 | b' ')
    {
        out.push(0, 3, SyntaxRole::Directive);
        i = skip_space(line, 3);
    }
    if at(line, i) == b'#' {
        out.push(i, line.len(), SyntaxRole::Comment);
        return LineState::START;
    }
    while at(line, i) == b'-' && matches!(at(line, i + 1), 0 | b' ') {
        out.push(i, i + 1, SyntaxRole::Punctuation);
        i = skip_space(line, i + 1);
    }
    if let Some(colon) = yaml_key(line, i) {
        out.push(i, colon, SyntaxRole::Key);
        out.push(colon, colon + 1, SyntaxRole::Punctuation);
        i = colon + 1;
    }
    yaml_value(line, i, indent, out)
}

/// Where the `:` ending a mapping key that starts at `from` sits, if one
/// does.
fn yaml_key(line: &[u8], from: usize) -> Option<usize> {
    let end = match at(line, from) {
        quote @ (b'"' | b'\'') => string_end(line, from + 1, quote, quote == b'"').0,
        _ => from,
    };
    let mut i = end;
    while i < line.len() {
        match line[i] {
            b':' if matches!(at(line, i + 1), 0 | b' ') => return (i > from).then_some(i),
            b'#' if i > 0 && line[i - 1] == b' ' => return None,
            b'[' | b'{' | b'"' | b'\'' if i == from => return None,
            _ => i += 1,
        }
    }
    None
}

/// Classify a YAML value from `from`, answering the state the next line
/// starts in (a block scalar opened here holds lines indented past
/// `indent`).
fn yaml_value(line: &[u8], from: usize, indent: usize, out: &mut Emit<'_>) -> LineState {
    let mut i = skip_space(line, from);
    let mut first = true;
    while i < line.len() {
        let byte = line[i];
        match byte {
            b'#' if i == 0 || line[i - 1] == b' ' => {
                out.push(i, line.len(), SyntaxRole::Comment);
                break;
            }
            b'|' | b'>' if first => {
                let end = i
                    + 1
                    + line[i + 1..]
                        .iter()
                        .take_while(|b| matches!(b, b'+' | b'-' | b'0'..=b'9'))
                        .count();
                out.push(i, end, SyntaxRole::Punctuation);
                let tail = skip_space(line, end);
                if at(line, tail) == b'#' {
                    out.push(tail, line.len(), SyntaxRole::Comment);
                }
                let parent = u32::try_from(indent).unwrap_or(0xff_ffff).min(0xff_ffff);
                return LineState::from_raw(YAML_BLOCK | (parent << 8));
            }
            b'"' | b'\'' => {
                out.push(i, i + 1, SyntaxRole::String);
                let (end, _) = quoted(line, i + 1, byte, out);
                i = end;
            }
            b'&' | b'*' | b'!' => {
                let end = i
                    + 1
                    + line[i + 1..]
                        .iter()
                        .take_while(|&&b| !matches!(b, b' ' | b',' | b']' | b'}'))
                        .count();
                out.push(i, end, SyntaxRole::Directive);
                i = end;
            }
            b'[' | b']' | b'{' | b'}' | b',' => {
                out.push(i, i + 1, SyntaxRole::Punctuation);
                i += 1;
            }
            b' ' | b'\t' => i = skip_space(line, i),
            _ => {
                let end = scalar_end(line, i).max(i + 1);
                out.push(i, end, scalar_role(&line[i..end]));
                i = end;
            }
        }
        first = false;
    }
    LineState::START
}

/// Where a plain YAML scalar starting at `from` ends: at a comment, or a
/// flow indicator.
fn scalar_end(line: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < line.len() {
        match line[i] {
            b'#' if i > 0 && line[i - 1] == b' ' => break,
            b',' | b']' | b'}' => break,
            _ => i += 1,
        }
    }
    let mut end = i;
    while end > from && line[end - 1] == b' ' {
        end -= 1;
    }
    end
}

/// A plain YAML scalar's role: a keyword it spells, a number, or text.
fn scalar_role(scalar: &[u8]) -> SyntaxRole {
    const WORDS: &[&[u8]] = &[
        b"true", b"True", b"TRUE", b"false", b"False", b"FALSE", b"null", b"Null", b"NULL", b"~",
        b"yes", b"no", b"on", b"off",
    ];
    if WORDS.contains(&scalar) {
        SyntaxRole::Keyword
    } else if is_number(scalar) {
        SyntaxRole::Number
    } else {
        SyntaxRole::Plain
    }
}

/// Whether `text` is a whole numeric literal.
fn is_number(text: &[u8]) -> bool {
    let digits = text
        .strip_prefix(b"-")
        .or_else(|| text.strip_prefix(b"+"))
        .unwrap_or(text);
    digits.first().is_some_and(u8::is_ascii_digit) && number_end(digits, 0) == digits.len()
}

/// A TOML multi-line basic string (`"""`) is open.
const TOML_BASIC: u32 = 1;
/// A TOML multi-line literal string (`'''`) is open.
const TOML_LITERAL: u32 = 2;

/// Classify one line of TOML.
pub(crate) fn toml(state: LineState, line: &[u8], out: &mut Emit<'_>) -> LineState {
    let from = match state.raw() {
        TOML_BASIC | TOML_LITERAL => {
            let close: &[u8] = if state.raw() == TOML_BASIC {
                b"\"\"\""
            } else {
                b"'''"
            };
            let Some(end) = find(line, 0, close) else {
                out.push(0, line.len(), SyntaxRole::String);
                return state;
            };
            out.push(0, end + 3, SyntaxRole::String);
            end + 3
        }
        _ => {
            let start = skip_space(line, 0);
            match at(line, start) {
                b'#' => {
                    out.push(start, line.len(), SyntaxRole::Comment);
                    return LineState::START;
                }
                b'[' => {
                    let close = find(line, start, b"]").map_or(line.len(), |close| {
                        if at(line, close + 1) == b']' {
                            close + 2
                        } else {
                            close + 1
                        }
                    });
                    out.push(start, close, SyntaxRole::Directive);
                    let tail = skip_space(line, close);
                    if at(line, tail) == b'#' {
                        out.push(tail, line.len(), SyntaxRole::Comment);
                    }
                    return LineState::START;
                }
                _ => start,
            }
        }
    };
    toml_tokens(line, from, out)
}

/// Classify TOML keys and values from `from`: a bare or quoted word before
/// `=` is a key, wherever it sits (a line's key or an inline table's).
fn toml_tokens(line: &[u8], from: usize, out: &mut Emit<'_>) -> LineState {
    let mut keys = KeyChain::default();
    let mut i = from;
    while i < line.len() {
        let byte = line[i];
        match byte {
            b' ' | b'\t' | b'\r' => i = skip_space(line, i),
            b'#' => {
                out.push(i, line.len(), SyntaxRole::Comment);
                break;
            }
            b'"' | b'\'' if starts_at(line, i, &[byte; 3]) => {
                let Some(close) = find(line, i + 3, &[byte; 3]) else {
                    out.push(i, line.len(), SyntaxRole::String);
                    let open = if byte == b'"' {
                        TOML_BASIC
                    } else {
                        TOML_LITERAL
                    };
                    return LineState::from_raw(open);
                };
                out.push(i, close + 3, SyntaxRole::String);
                i = close + 3;
            }
            b'"' | b'\'' => {
                let (end, closed) = string_end(line, i + 1, byte, byte == b'"');
                if closed && keys.holds(line, i, end) {
                    out.push(i, end, SyntaxRole::Key);
                } else if byte == b'"' {
                    out.push(i, i + 1, SyntaxRole::String);
                    quoted(line, i + 1, b'"', out);
                } else {
                    out.push(i, end, SyntaxRole::String);
                }
                i = end;
            }
            b'=' | b'.' | b',' | b'[' | b']' | b'{' | b'}' => {
                out.push(i, i + 1, SyntaxRole::Punctuation);
                i += 1;
            }
            _ if is_ident_continue(byte) || matches!(byte, b'-' | b'+') => {
                let end = bare_end(line, i);
                let word = &line[i..end];
                let role = if keys.holds(line, i, end) {
                    SyntaxRole::Key
                } else if matches!(word, b"true" | b"false") {
                    SyntaxRole::Keyword
                } else if word
                    .first()
                    .is_some_and(|b| b.is_ascii_digit() || matches!(b, b'-' | b'+'))
                    || matches!(
                        word,
                        b"inf" | b"nan" | b"+inf" | b"-inf" | b"+nan" | b"-nan"
                    )
                {
                    SyntaxRole::Number
                } else {
                    SyntaxRole::Plain
                };
                out.push(i, end, role);
                i = end.max(i + 1);
            }
            _ => {
                out.push(i, i + 1, SyntaxRole::Error);
                i += 1;
            }
        }
    }
    LineState::START
}

/// Which words of a line are keys. A dotted key's parts share one answer,
/// found once for the whole chain, so a long chain is not rescanned from
/// each of its words.
#[derive(Default)]
struct KeyChain {
    /// The chain last decided ends before this byte.
    until: usize,
    keyed: bool,
}

impl KeyChain {
    /// Whether the word `line[start..end]` is (a part of) a key.
    fn holds(&mut self, line: &[u8], start: usize, end: usize) -> bool {
        if start >= self.until {
            (self.until, self.keyed) = key_follows(line, end);
        }
        self.keyed
    }
}

/// Whether what follows `from` finishes a key — more `.`-separated parts,
/// bare or quoted, and then its `=` — and the byte that decided it.
fn key_follows(line: &[u8], from: usize) -> (usize, bool) {
    let mut i = from;
    loop {
        i = skip_space(line, i);
        match at(line, i) {
            b'=' => return (i, true),
            b'.' => {
                i = skip_space(line, i + 1);
                i = match at(line, i) {
                    quote @ (b'"' | b'\'') => match string_end(line, i + 1, quote, quote == b'"') {
                        (end, true) => end,
                        (_, false) => return (i, false),
                    },
                    byte if is_ident_continue(byte) || byte == b'-' => bare_end(line, i),
                    _ => return (i, false),
                };
            }
            _ => return (i, false),
        }
    }
}

/// Where a bare TOML word — a key, a number, a date — starting at `from`
/// ends.
fn bare_end(line: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < line.len() {
        let byte = line[i];
        let date_or_number = matches!(byte, b':' | b'+' | b'-')
            || (byte == b'.'
                && at(line, i + 1).is_ascii_digit()
                && at(line, from).is_ascii_digit());
        if is_ident_continue(byte) || date_or_number {
            i += 1;
        } else {
            break;
        }
    }
    i
}
